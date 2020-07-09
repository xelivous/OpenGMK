//! GameMaker 8 sound system.

use rodio::{Device, Sink, Source};
use serde::{Deserialize, Serialize};
use std::{alloc, collections::HashMap, mem, slice, str, sync::Arc, time::Duration};

pub struct AudioSystem {
    current_mp3: Option<(AudioHandle, Sink)>,
    device: Option<Device>,
    wave_sinks: HashMap<AudioHandle, Sink>,

    assets: HashMap<AudioHandle, AudioAsset>,
    next_asset_handle: u64,
}

#[derive(Copy, Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct AudioHandle(u64);

enum AudioAsset {
    MP3(MP3Stream),
    Wave(WaveStream),
    Midi(/* good joke */),
}

struct MP3Stream {
    channels: u16,
    duration: Duration,
    sample_rate: u32,

    frame_buf: Box<[rmp3::Sample; rmp3::MAX_SAMPLES_PER_FRAME]>,
    frame_samples: &'static [rmp3::Sample],
    frame_ofs: usize,

    _data: Arc<Box<[u8]>>,
    data_ref: &'static [u8],
    data_ofs: usize,

    decoder: rmp3::Decoder,
}

fn mp3_uninit_framebuf() -> Box<[rmp3::Sample; rmp3::MAX_SAMPLES_PER_FRAME]> {
    type BufferTy = [rmp3::Sample; rmp3::MAX_SAMPLES_PER_FRAME];

    unsafe {
        let memory = alloc::alloc(alloc::Layout::new::<BufferTy>()) as *mut BufferTy;
        Box::from_raw(memory)
    }
}

impl Clone for MP3Stream {
    fn clone(&self) -> Self {
        Self {
            channels: self.channels,
            sample_rate: self.sample_rate,
            duration: self.duration,

            frame_buf: mp3_uninit_framebuf(),
            frame_samples: &[],
            frame_ofs: 0,

            _data: self._data.clone(),
            data_ref: self.data_ref,
            data_ofs: 0,

            decoder: rmp3::Decoder::new(),
        }
    }
}

impl MP3Stream {
    pub fn new(data: Arc<Box<[u8]>>) -> Option<Self> {
        use rmp3::{DecoderStream, Frame, Samples};

        let mut decoder = DecoderStream::new(data.as_ref());

        let mut o_channels: Option<u16> = None;
        let mut o_sample_rate: Option<u32> = None;
        let mut length = 0.0f64;

        while let Ok(frame) = decoder.peek() {
            match frame {
                Frame::Audio(Samples { channels, sample_rate, sample_count, .. }) => {
                    if ensure(&mut o_channels, channels as u16) && ensure(&mut o_sample_rate, sample_rate) {
                        length += sample_count as f64 / sample_rate as f64;
                    }
                },
                _ => (),
            }
            let _ = decoder.skip();
        }

        let channels = o_channels?;
        let sample_rate = o_sample_rate?;

        let duration = Duration::from_secs_f64(length);

        let frame_buf = mp3_uninit_framebuf();

        let data_ref = unsafe { slice::from_raw_parts(data.as_ptr(), data.len()) };

        Some(Self {
            channels,
            duration,
            sample_rate,

            frame_buf,
            frame_samples: &[],
            frame_ofs: 0,

            _data: data,
            data_ref,
            data_ofs: 0,

            decoder: rmp3::Decoder::new(),
        })
    }

    fn refill(&mut self) -> bool {
        use rmp3::{Frame, Samples};

        loop {
            match self.decoder.peek(&self.data_ref[self.data_ofs..]) {
                Ok(Frame::Audio(Samples { bytes_consumed, channels, sample_rate, .. })) => {
                    if channels as u16 == self.channels && sample_rate == self.sample_rate {
                        match self.decoder.next(&self.data_ref[self.data_ofs..], self.frame_buf.as_mut()) {
                            Ok(Frame::Audio(samples)) => {
                                self.frame_samples = unsafe { mem::transmute(samples.samples) }; // mmm
                                self.frame_ofs = 0;
                                self.data_ofs += bytes_consumed;
                                break true
                            },
                            _ => unreachable!(),
                        }
                    } else {
                        self.data_ofs += bytes_consumed;
                    }
                },
                Ok(Frame::Unknown { bytes_consumed, .. }) => {
                    self.data_ofs += bytes_consumed;
                },
                Err(rmp3::InsufficientData) => break false,
            }
        }
    }
}

impl Iterator for MP3Stream {
    type Item = rmp3::Sample;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self.frame_samples.get(self.frame_ofs).copied() {
            Some(sample) => {
                self.frame_ofs += 1;
                Some(sample)
            },
            None => {
                if self.refill() {
                    self.next() // possible SO
                } else {
                    None
                }
            },
        }
    }
}

impl Source for MP3Stream {
    #[inline]
    fn current_frame_len(&self) -> Option<usize> {
        Some(self.frame_samples.len())
    }

    #[inline]
    fn channels(&self) -> u16 {
        self.channels
    }

    #[inline]
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[inline]
    fn total_duration(&self) -> Option<Duration> {
        Some(self.duration)
    }
}

enum WaveStream {
    Int16(PCMSource<i16>),
    Float(PCMSource<f32>),
}

impl WaveStream {
    pub fn new(data: Arc<Box<[u8]>>) -> Option<Self> {
        let data2 = data.clone(); // shut the fuck up
        let (hdr, mut riff) = riff_chunk(data2.as_ref())?;
        if hdr != "RIFF" {
            return None
        }
        let mut offset = 8 + 4;

        riff = &riff[4..]; // 'WAVE'

        let mut format_tag: Option<u16> = None;
        let mut channels: Option<u16> = None;
        let mut sample_rate: Option<u32> = None;
        let mut bits_per_sample: Option<u16> = None;

        loop {
            let (header, chunk) = riff_chunk(riff)?;
            match header {
                "fmt " => {
                    if chunk.len() < 16 {
                        return None
                    }
                    format_tag = Some(u16::from_le_bytes([chunk[0], chunk[1]]));
                    channels = Some(u16::from_le_bytes([chunk[2], chunk[3]]));
                    sample_rate = Some(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
                    bits_per_sample = Some(u16::from_le_bytes([chunk[14], chunk[15]]));
                },
                "data" => {
                    let (bps, fmt, channels, sample_rate) = (bits_per_sample?, format_tag?, channels?, sample_rate?);
                    match fmt {
                        1 => {
                            assert_eq!(bps, 16); // It can technically be other than i16. TODO?
                            break Some(WaveStream::Int16(PCMSource::new(
                                data,
                                offset + 8,
                                chunk.len(),
                                channels,
                                sample_rate,
                            )))
                        },
                        3 => {
                            assert_eq!(bps, 32); // Maybe this can be 64? Don't care.
                            break Some(WaveStream::Float(PCMSource::new(
                                data,
                                offset + 8,
                                chunk.len(),
                                channels,
                                sample_rate,
                            )))
                        },
                        6 => unimplemented!("alaw unimplemented"),
                        7 => unimplemented!("mulaw unimplemented"),
                        _ => return None,
                    }
                },
                _ => (),
            }
            offset += 8 + chunk.len();
            riff = &riff[8 + chunk.len()..];
        }
    }
}

struct PCMSource<T: 'static> {
    channels: u16,
    duration: Duration,
    sample_rate: u32,

    _data: Arc<Box<[u8]>>,
    pcm: &'static [T],
    ofs: usize,
}

impl<T> Clone for PCMSource<T> {
    fn clone(&self) -> Self {
        Self { _data: self._data.clone(), ofs: 0, ..*self }
    }
}

impl<T: rodio::Sample + 'static> PCMSource<T> {
    pub fn new(data: Arc<Box<[u8]>>, byte_ofs: usize, byte_len: usize, channels: u16, sample_rate: u32) -> Self {
        let pcm: &'static [T] = unsafe {
            slice::from_raw_parts(data.as_ptr().offset(byte_ofs as isize).cast(), byte_len / mem::size_of::<T>())
        };
        let duration = Duration::from_secs_f64(pcm.len() as f64 / channels as f64 / sample_rate as f64);

        Self { channels, duration, sample_rate, _data: data, pcm, ofs: 0 }
    }
}

impl<T: rodio::Sample + 'static> Iterator for PCMSource<T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.pcm.get(self.ofs).copied()?;
        self.ofs += 1;
        Some(sample)
    }
}

impl<T: rodio::Sample + 'static> Source for PCMSource<T> {
    #[inline]
    fn current_frame_len(&self) -> Option<usize> {
        Some(self.pcm.len())
    }

    #[inline]
    fn channels(&self) -> u16 {
        self.channels
    }

    #[inline]
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[inline]
    fn total_duration(&self) -> Option<Duration> {
        Some(self.duration)
    }
}

impl AudioSystem {
    pub fn new() -> Self {
        Self {
            assets: HashMap::new(),
            current_mp3: None,
            device: rodio::default_output_device(),
            wave_sinks: HashMap::with_capacity(4),
            next_asset_handle: 0,
        }
    }

    pub fn play(&mut self, sound: AudioHandle) {
        let device = match &self.device {
            Some(device) => device,
            None => return,
        };

        match self.assets.get(&sound) {
            Some(AudioAsset::MP3(mp3)) => {
                let sink = match self.current_mp3.take() {
                    Some((_, sink)) => {
                        sink.stop();
                        sink
                    },
                    None => Sink::new(device),
                };
                sink.append(mp3.clone());
                self.current_mp3 = Some((sound, sink));
            },
            Some(AudioAsset::Wave(wave)) => {
                let sink = Sink::new(device);
                match wave {
                    WaveStream::Int16(ipcm) => sink.append(ipcm.clone()),
                    WaveStream::Float(fpcm) => sink.append(fpcm.clone()),
                }
                self.wave_sinks.insert(sound, sink);
            },
            Some(_) => unimplemented!(),
            _ => (),
        }
    }

    pub fn register_mp3(&mut self, data: impl Into<Box<[u8]>>) -> Option<AudioHandle> {
        let stream = MP3Stream::new(data.into().into())?;
        let id = self.next_asset_handle;
        self.next_asset_handle += 1;
        self.assets.insert(AudioHandle(id), AudioAsset::MP3(stream));
        Some(AudioHandle(id))
    }

    pub fn register_wav(&mut self, data: impl Into<Box<[u8]>>) -> Option<AudioHandle> {
        let stream = WaveStream::new(data.into().into())?;
        let id = self.next_asset_handle;
        self.next_asset_handle += 1;
        self.assets.insert(AudioHandle(id), AudioAsset::Wave(stream));
        Some(AudioHandle(id))
    }
}

fn riff_chunk<'a>(data: &'a [u8]) -> Option<(&'a str, &'a [u8])> {
    let header = data.get(..4).and_then(|bs| str::from_utf8(bs).ok())?;
    let length = data.get(4..8).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))?;
    Some((header, data.get(8..8 + length as usize)?))
}

#[inline(always)]
#[must_use]
fn ensure<T: Copy + Clone + Eq>(o: &mut Option<T>, val: T) -> bool {
    match o {
        Some(v) => *v == val,
        n @ None => {
            *n = Some(val);
            true
        },
    }
}
