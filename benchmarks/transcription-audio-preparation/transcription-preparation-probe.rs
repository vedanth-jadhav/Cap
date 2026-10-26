use cap_audio::high_quality_resampler_options;
use cap_editor::{TranscriptionAudioSource, TranscriptionAudioTake, append_transcription_audio};
use cap_project::{RecordingMeta, StudioRecordingMeta};
use cap_rendering::Video;
use ffmpeg::{ChannelLayout, codec as avcodec, format as avformat, software::resampling};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Instant,
};
const DECODE_SAMPLE_RATE: u32 = 48_000;
const WHISPER_SAMPLE_RATE: u32 = 16_000;
static PCM_CHUNK: AtomicUsize = AtomicUsize::new(1024);
static USE_CORE_NORMALIZER: AtomicBool = AtomicBool::new(false);
fn decode_audio_file(path: &Path) -> Result<(Vec<f32>, usize), String> {
    let mut input_ctx = avformat::input(&path).map_err(|e| format!("Input Open / {e}"))?;
    let input_stream = input_ctx
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .ok_or_else(|| "No Stream".to_string())?;

    let decoder_ctx = avcodec::Context::from_parameters(input_stream.parameters())
        .map_err(|e| format!("Decoder Parameters / {e}"))?;
    let mut decoder = decoder_ctx
        .decoder()
        .audio()
        .map_err(|e| format!("Set Parameters / {e}"))?;

    let source_channels = decoder.channels().max(1);
    if decoder.channel_layout().is_empty() {
        decoder.set_channel_layout(ChannelLayout::default(i32::from(source_channels)));
    }
    decoder.set_packet_time_base(input_stream.time_base());

    let target_channels: u16 = if source_channels <= 1 { 1 } else { 2 };
    let target_channel_layout = ChannelLayout::default(i32::from(target_channels));
    let mut resampler = resampling::Context::get_with(
        decoder.format(),
        decoder.channel_layout(),
        decoder.rate(),
        avformat::Sample::F32(avformat::sample::Type::Packed),
        target_channel_layout,
        DECODE_SAMPLE_RATE,
        high_quality_resampler_options(),
    )
    .map_err(|e| format!("Resampler / {e}"))?;

    let index = input_stream.index();

    let mut decoded_frame = ffmpeg::frame::Audio::empty();
    let mut samples: Vec<f32> = Vec::new();

    for (stream, packet) in input_ctx.packets() {
        if stream.index() != index {
            continue;
        }

        decoder
            .send_packet(&packet)
            .map_err(|e| format!("Send Packet / {e}"))?;

        while decoder.receive_frame(&mut decoded_frame).is_ok() {
            run_decode_resampler(&mut resampler, &decoded_frame, &mut samples)?;
        }
    }

    decoder.send_eof().map_err(|e| format!("Send EOF / {e}"))?;

    while decoder.receive_frame(&mut decoded_frame).is_ok() {
        run_decode_resampler(&mut resampler, &decoded_frame, &mut samples)?;
    }

    flush_decode_resampler(&mut resampler, &mut samples)?;

    Ok((samples, usize::from(target_channels)))
}

fn run_decode_resampler(
    resampler: &mut resampling::Context,
    decoded_frame: &ffmpeg::frame::Audio,
    samples: &mut Vec<f32>,
) -> Result<(), String> {
    let target = *resampler.output();
    let capacity = decode_resample_capacity(resampler, decoded_frame.samples());
    let mut resampled_frame =
        ffmpeg::frame::Audio::new(target.format, capacity, target.channel_layout);

    resampler
        .run(decoded_frame, &mut resampled_frame)
        .map_err(|e| format!("Run Resampler / {e}"))?;

    append_resampled_frame(samples, &resampled_frame)
}

fn flush_decode_resampler(
    resampler: &mut resampling::Context,
    samples: &mut Vec<f32>,
) -> Result<(), String> {
    for _ in 0..64 {
        let Some(delay) = resampler.delay() else {
            break;
        };
        let target = *resampler.output();
        let capacity = delay
            .output
            .max(1)
            .saturating_add(16)
            .min(i64::from(i32::MAX)) as usize;
        let mut resampled_frame =
            ffmpeg::frame::Audio::new(target.format, capacity, target.channel_layout);
        let remaining = resampler
            .flush(&mut resampled_frame)
            .map_err(|e| format!("Flush Resampler / {e}"))?;

        let output_samples = resampled_frame.samples();
        append_resampled_frame(samples, &resampled_frame)?;

        if remaining.is_none() || output_samples == 0 {
            break;
        }
    }
    Ok(())
}

fn decode_resample_capacity(resampler: &resampling::Context, input_samples: usize) -> usize {
    let src_rate = u64::from(resampler.input().rate.max(1));
    let dst_rate = u64::from(resampler.output().rate.max(1));
    let pending_output_samples = resampler
        .delay()
        .map(|d| d.output.max(0) as u64)
        .unwrap_or(0);
    let resampled_from_input = (input_samples as u64)
        .saturating_mul(dst_rate)
        .div_ceil(src_rate);

    pending_output_samples
        .saturating_add(resampled_from_input)
        .saturating_add(16)
        .min(i32::MAX as u64) as usize
}

fn append_resampled_frame(
    samples: &mut Vec<f32>,
    frame: &ffmpeg::frame::Audio,
) -> Result<(), String> {
    if frame.samples() == 0 {
        return Ok(());
    }

    let byte_len = frame
        .samples()
        .saturating_mul(frame.channels() as usize)
        .saturating_mul(std::mem::size_of::<f32>());
    let data = frame
        .data(0)
        .get(..byte_len)
        .ok_or_else(|| "Resampled frame data shorter than expected".to_string())?;

    // SAFETY: ffmpeg's frame buffers are allocated with av_malloc alignment,
    // and the slice length is a whole number of f32s by construction.
    samples
        .extend(unsafe { std::slice::from_raw_parts(data.as_ptr() as *const f32, byte_len / 4) });
    Ok(())
}

fn normalize_audio_for_transcription(samples: &mut [f32]) -> f32 {
    if USE_CORE_NORMALIZER.load(Ordering::Relaxed) {
        return cap_audio::normalize_for_transcription(samples);
    }
    if samples.is_empty() {
        return 1.0;
    }

    let peak = samples
        .iter()
        .fold(0.0_f32, |max, sample| max.max(sample.abs()));
    if peak <= f32::EPSILON {
        return 1.0;
    }

    let rms =
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt();
    if rms <= f32::EPSILON {
        return 1.0;
    }

    let target_rms = 0.08_f32;
    let desired_gain = (target_rms / rms).clamp(1.0, 8.0);
    let peak_limited_gain = 0.98 / peak;
    let gain = desired_gain.min(peak_limited_gain);

    if (gain - 1.0).abs() > 0.01 {
        for sample in samples {
            *sample = (*sample * gain).clamp(-0.98, 0.98);
        }
    }

    gain
}

fn extract_project_audio(project_path: &Path, output_path: &Path) -> Result<(), String> {
    ffmpeg::init().map_err(|e| format!("Failed to initialise ffmpeg: {e}"))?;

    let meta = RecordingMeta::load_for_project(project_path)
        .map_err(|e| format!("Failed to read recording metadata: {e}"))?;
    let Some(studio) = meta.studio_meta() else {
        return Err("Only studio recordings can be transcribed".to_string());
    };

    studio.ensure_ordinary_media_access(&meta.project_path)?;
    let mut final_samples = Vec::new();
    let mut any_audio = false;
    match studio {
        StudioRecordingMeta::SingleSegment { segment } => {
            let display_duration = Video::new(
                segment.display.path.to_path(&meta.project_path),
                segment.display.start_time.unwrap_or_default(),
            )
            .map_err(|error| format!("Failed to read display video: {error}"))?
            .duration;
            let mut sources = Vec::new();
            if let Some(audio) = &segment.audio {
                let path = meta.path(&audio.path);
                if path.exists() {
                    match decode_audio_file(&path) {
                        Ok((samples, channels)) => {
                            any_audio = true;
                            sources.push(TranscriptionAudioSource {
                                samples,
                                channels,
                                offset_secs: 0.0,
                            });
                        }
                        Err(error) => tracing::warn!(
                            path = %path.display(),
                            "Failed to process audio source: {error}"
                        ),
                    }
                }
            }
            let take = TranscriptionAudioTake {
                display_duration_secs: display_duration,
                sources,
            };
            append_transcription_audio(&mut final_samples, &take, DECODE_SAMPLE_RATE)
                .map_err(|error| format!("Failed to assemble transcription audio: {error}"))?;
        }
        StudioRecordingMeta::MultipleSegments { inner } => {
            for (segment_idx, segment) in inner.segments.iter().enumerate() {
                let display_duration = Video::new(
                    segment.display.path.to_path(&meta.project_path),
                    segment.display.start_time.unwrap_or_default(),
                )
                .map_err(|error| {
                    format!("Failed to read display video for segment {segment_idx}: {error}")
                })?
                .duration;
                let offsets = segment.calculate_audio_offsets();
                let mut sources = Vec::new();
                let mut push_source = |audio: &cap_project::AudioMeta, offset_secs: f64| {
                    let path = meta.path(&audio.path);
                    if !path.exists() {
                        return;
                    }
                    match decode_audio_file(&path) {
                        Ok((samples, channels)) => {
                            any_audio = true;
                            sources.push(TranscriptionAudioSource {
                                samples,
                                channels,
                                offset_secs,
                            });
                        }
                        Err(error) => tracing::warn!(
                            path = %path.display(),
                            "Failed to process audio source: {error}"
                        ),
                    }
                };

                if let Some(system_audio) = &segment.system_audio {
                    push_source(system_audio, f64::from(offsets.system_audio));
                }
                if let Some(mic) = &segment.mic {
                    push_source(mic, f64::from(offsets.mic));
                }
                let take = TranscriptionAudioTake {
                    display_duration_secs: display_duration,
                    sources,
                };
                append_transcription_audio(&mut final_samples, &take, DECODE_SAMPLE_RATE)
                    .map_err(|error| format!("Failed to assemble transcription audio: {error}"))?;
            }
        }
    }

    if !any_audio {
        return Err("No audio sources found in the recording metadata".to_string());
    }

    if final_samples.is_empty() {
        return Err("Failed to process any audio sources".to_string());
    }

    let gain = normalize_audio_for_transcription(&mut final_samples);
    if (gain - 1.0).abs() > 0.01 {
        tracing::info!("Applied transcription audio gain: {gain:.2}x");
    }

    write_wav_16k_mono(&final_samples, output_path)
}

fn write_wav_16k_mono(mixed_samples: &[f32], output_path: &Path) -> Result<(), String> {
    let mut output =
        avformat::output(&output_path).map_err(|e| format!("Failed to create output file: {e}"))?;

    let codec = avcodec::encoder::find_by_name("pcm_s16le")
        .ok_or_else(|| "PCM encoder not found".to_string())?;

    let mut encoder = avcodec::Context::new()
        .encoder()
        .audio()
        .map_err(|e| format!("Failed to create encoder: {e}"))?;

    encoder.set_rate(WHISPER_SAMPLE_RATE as i32);
    let channel_layout = ChannelLayout::MONO;
    encoder.set_channel_layout(channel_layout);
    encoder.set_format(avformat::Sample::I16(avformat::sample::Type::Packed));

    let mut encoder = encoder
        .open_as(codec)
        .map_err(|e| format!("Failed to open encoder: {e}"))?;

    let mut stream = output
        .add_stream(codec)
        .map_err(|e| format!("Failed to add stream: {e}"))?;
    stream.set_parameters(&encoder);

    output
        .write_header()
        .map_err(|e| format!("Failed to write header: {e}"))?;

    let mut resampler = resampling::Context::get(
        avformat::Sample::F32(avformat::sample::Type::Packed),
        channel_layout,
        DECODE_SAMPLE_RATE,
        avformat::Sample::I16(avformat::sample::Type::Packed),
        channel_layout,
        WHISPER_SAMPLE_RATE,
    )
    .map_err(|e| format!("Failed to create resampler: {e}"))?;

    let frame_size = encoder.frame_size() as usize;
    let frame_size = if frame_size == 0 {
        PCM_CHUNK.load(Ordering::Relaxed)
    } else {
        frame_size
    };

    for chunk in mixed_samples.chunks(frame_size) {
        let mut input_frame = ffmpeg::frame::Audio::new(
            avformat::Sample::F32(avformat::sample::Type::Packed),
            chunk.len(),
            channel_layout,
        );
        input_frame.set_rate(DECODE_SAMPLE_RATE);

        // SAFETY: reinterpreting the f32 chunk as its little-endian bytes for
        // the frame copy, exactly as the Tauri encoder does.
        let bytes = unsafe {
            std::slice::from_raw_parts(chunk.as_ptr() as *const u8, std::mem::size_of_val(chunk))
        };
        input_frame.data_mut(0)[0..bytes.len()].copy_from_slice(bytes);

        let mut output_frame = ffmpeg::frame::Audio::new(
            avformat::Sample::I16(avformat::sample::Type::Packed),
            frame_size,
            ChannelLayout::MONO,
        );
        output_frame.set_rate(WHISPER_SAMPLE_RATE);

        if let Err(error) = resampler.run(&input_frame, &mut output_frame) {
            tracing::error!("Failed to resample chunk: {error}");
            continue;
        }

        if let Err(error) = encoder.send_frame(&output_frame) {
            tracing::error!("Failed to send frame to encoder: {error}");
            continue;
        }

        loop {
            let mut packet = ffmpeg::Packet::empty();
            if encoder.receive_packet(&mut packet).is_err() {
                break;
            }
            if let Err(error) = packet.write_interleaved(&mut output) {
                tracing::error!("Failed to write packet: {error}");
            }
        }
    }

    encoder
        .send_eof()
        .map_err(|e| format!("Failed to send EOF: {e}"))?;

    loop {
        let mut packet = ffmpeg::Packet::empty();
        if encoder.receive_packet(&mut packet).is_err() {
            break;
        }
        packet
            .write_interleaved(&mut output)
            .map_err(|e| format!("Failed to write final packet: {e}"))?;
    }

    output
        .write_trailer()
        .map_err(|e| format!("Failed to write trailer: {e}"))
}
fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap();
    let root = PathBuf::from(args.next().unwrap());
    let output = PathBuf::from(args.next().unwrap());
    assert!(!output.exists());
    if mode == "batch" || mode == "core-batch" {
        PCM_CHUNK.store(16384, Ordering::Relaxed);
    }
    USE_CORE_NORMALIZER.store(mode == "core" || mode == "core-batch", Ordering::Relaxed);
    let start = Instant::now();
    extract_project_audio(&root, &output).unwrap();
    println!(
        "AUDIO_PREP {}",
        serde_json::json!({"mode":mode,"total_seconds":start.elapsed().as_secs_f64(),"bytes":std::fs::metadata(output).unwrap().len()})
    );
}
