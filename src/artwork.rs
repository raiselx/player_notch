use crate::theme::Theme;
use eframe::egui::ColorImage;
use std::{
    io::Cursor,
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};
use windows::{
    Foundation::{AsyncStatus, IAsyncOperation},
    Storage::Streams::{
        DataReader, IRandomAccessStreamReference, IRandomAccessStreamWithContentType,
    },
    core::{Interface, RuntimeType},
};

const MAX_ARTWORK_BYTES: u64 = 15 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub struct Artwork {
    pub image: ColorImage,
    pub theme: Theme,
}

impl Artwork {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let image = reader.decode().ok()?;
        let side = image.width().min(image.height());
        let cropped = image.crop_imm(
            (image.width() - side) / 2,
            (image.height() - side) / 2,
            side,
            side,
        );
        let cover = cropped.thumbnail(176, 176);
        let theme = Theme::from_image(&cover);
        let rgba = cover.into_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        Some(Self {
            image: ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
            theme,
        })
    }
}

pub struct ArtworkLoader {
    track: [String; 4],
    track_started: Instant,
    next_read: Instant,
    pending: Option<ThumbnailRead>,
    current: Option<Arc<Artwork>>,
    cached: Option<(Vec<u8>, Option<Arc<Artwork>>)>,
}

impl ArtworkLoader {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            track: Default::default(),
            track_started: now,
            next_read: now,
            pending: None,
            current: None,
            cached: None,
        }
    }

    pub fn update(
        &mut self,
        track: [&str; 4],
        thumbnail: Option<IRandomAccessStreamReference>,
        now: Instant,
    ) -> Option<Arc<Artwork>> {
        let Some(thumbnail) = thumbnail else {
            self.clear();
            return None;
        };
        let previous = self.track.each_ref().map(String::as_str);
        if track != previous {
            let [app, _, artist, album] = track;
            let [previous_app, _, previous_artist, previous_album] = previous;
            let same_album = !album.is_empty()
                && (app, artist, album) == (previous_app, previous_artist, previous_album);
            self.cancel_pending();
            if !same_album {
                self.current = None;
            }
            self.track = track.map(str::to_owned);
            self.track_started = now;
            self.next_read = now;
        }
        if self.pending.is_none() && now >= self.next_read {
            self.pending = ThumbnailRead::start(&thumbnail, now);
            // Some players publish the new track before updating its thumbnail.
            let interval = if now.duration_since(self.track_started) < READ_TIMEOUT {
                Duration::from_millis(250)
            } else if self.current.is_none() {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(4)
            };
            self.next_read = now + interval;
        }
        if let Some(read) = &mut self.pending {
            if let Poll::Ready(bytes) = read.poll(now) {
                self.pending = None;
                if let Some(bytes) = bytes {
                    if self.cached.as_ref().is_none_or(|(old, _)| *old != bytes) {
                        let artwork = Artwork::decode(&bytes).map(Arc::new);
                        self.cached = Some((bytes, artwork));
                    }
                    self.current = self.cached.as_ref().and_then(|(_, art)| art.clone());
                }
            }
        }
        self.current.clone()
    }

    pub fn clear(&mut self) {
        self.cancel_pending();
        self.current = None;
        self.track = Default::default();
    }

    pub fn reset(&mut self) {
        self.clear();
        self.cached = None;
    }

    pub fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    pub fn refresh(&mut self, now: Instant) {
        self.cancel_pending();
        self.next_read = now;
    }

    fn cancel_pending(&mut self) {
        if let Some(read) = self.pending.take() {
            read.cancel();
        }
    }
}

enum ThumbnailStage {
    Opening(IAsyncOperation<IRandomAccessStreamWithContentType>),
    Reading {
        reader: DataReader,
        op: IAsyncOperation<u32>,
    },
}

struct ThumbnailRead {
    stage: ThumbnailStage,
    deadline: Instant,
}

impl ThumbnailRead {
    fn start(thumbnail: &IRandomAccessStreamReference, now: Instant) -> Option<Self> {
        Some(Self {
            stage: ThumbnailStage::Opening(thumbnail.OpenReadAsync().ok()?),
            deadline: now + READ_TIMEOUT,
        })
    }

    fn poll(&mut self, now: Instant) -> Poll<Option<Vec<u8>>> {
        if now >= self.deadline {
            self.cancel();
            return Poll::Ready(None);
        }
        match &self.stage {
            ThumbnailStage::Opening(op) => match completed(op) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(stream) => {
                    let Some((reader, op)) = stream.and_then(start_read) else {
                        return Poll::Ready(None);
                    };
                    self.stage = ThumbnailStage::Reading { reader, op };
                    self.poll(now)
                }
            },
            ThumbnailStage::Reading { reader, op } => match completed(op) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(size) => Poll::Ready(size.and_then(|size| read_bytes(reader, size))),
            },
        }
    }

    fn cancel(&self) {
        let _ = match &self.stage {
            ThumbnailStage::Opening(op) => op.Cancel(),
            ThumbnailStage::Reading { op, .. } => op.Cancel(),
        };
    }
}

fn completed<T: RuntimeType + 'static>(op: &IAsyncOperation<T>) -> Poll<Option<T>> {
    match op.Status() {
        Ok(AsyncStatus::Started) => Poll::Pending,
        Ok(AsyncStatus::Completed) => Poll::Ready(op.GetResults().ok()),
        _ => Poll::Ready(None),
    }
}

fn start_read(
    stream: IRandomAccessStreamWithContentType,
) -> Option<(DataReader, IAsyncOperation<u32>)> {
    let size = stream.Size().ok()?;
    if !(1..=MAX_ARTWORK_BYTES).contains(&size) {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
    let op = reader.LoadAsync(size as u32).ok()?.cast().ok()?;
    Some((reader, op))
}

fn read_bytes(reader: &DataReader, size: u32) -> Option<Vec<u8>> {
    if !(1..=MAX_ARTWORK_BYTES).contains(&u64::from(size)) {
        return None;
    }
    let mut bytes = vec![0; size as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::thread;
    use windows::{
        Storage::Streams::{DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference},
        Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    };

    fn thumbnail(color: [u8; 3]) -> IRandomAccessStreamReference {
        let image = ImageBuffer::from_pixel(8, 8, Rgb(color));
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, ImageFormat::Png).unwrap();
        let writer = DataWriter::new().unwrap();
        writer.WriteBytes(png.get_ref()).unwrap();
        let stream = InMemoryRandomAccessStream::new().unwrap();
        stream
            .WriteAsync(&writer.DetachBuffer().unwrap())
            .unwrap()
            .get()
            .unwrap();
        stream.Seek(0).unwrap();
        RandomAccessStreamReference::CreateFromStream(&stream)
            .unwrap()
            .cast()
            .unwrap()
    }

    fn load(
        loader: &mut ArtworkLoader,
        track: [&str; 4],
        thumbnail: IRandomAccessStreamReference,
    ) -> Arc<Artwork> {
        let started = Instant::now();
        loop {
            if let Some(art) = loader.update(track, Some(thumbnail.clone()), Instant::now()) {
                return art;
            }
            assert!(started.elapsed() < READ_TIMEOUT, "artwork did not load");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn consecutive_tracks_can_use_the_same_cover_and_missing_cover_clears_it() {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.unwrap();
        {
            let mut loader = ArtworkLoader::new();
            let red = [255, 0, 0];
            let first = load(&mut loader, ["app", "first", "artist", "A"], thumbnail(red));
            let second = loader
                .update(
                    ["app", "second", "artist", "A"],
                    Some(thumbnail(red)),
                    Instant::now(),
                )
                .expect("same album cover should stay visible on the first poll");
            assert!(Arc::ptr_eq(&first, &second));

            let third = load(&mut loader, ["app", "third", "artist", "B"], thumbnail(red));
            assert!(Arc::ptr_eq(&first, &third));

            let blue = [0, 0, 255];
            let final_track = ["app", "fourth", "artist", "C"];
            let fourth = load(&mut loader, final_track, thumbnail(blue));
            assert_eq!(fourth.image.pixels[0], Color32::BLUE);
            assert!(loader.update(final_track, None, Instant::now()).is_none());
        }
        unsafe { RoUninitialize() };
    }
}
