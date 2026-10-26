
extern crate normalization_native;
fn normalize_audio_for_transcription(samples: &mut [f32]) -> f32 {
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
fn equal(a:f32,b:f32){assert!((a.is_nan()&&b.is_nan())||a.to_bits()==b.to_bits(),"{a:?} != {b:?}");}
#[test]
fn optimization_levels_preserve_normalization(){
 for len in [0,1,2,3,15,16,17,1023,1024,1025,4097] {
  for amplitude in [0.0f32,1.0e-8,0.001,0.01,0.08,0.5,1.0,4.0] {
   let mut state=0x1947_cdefu32;let mut before:Vec<f32>=(0..len).map(|_|{state^=state<<13;state^=state>>17;state^=state<<5;(state as i32 as f32)/(i32::MAX as f32)*amplitude}).collect();
   let mut after=before.clone();let gain=normalize_audio_for_transcription(&mut before);let other=normalization_native::normalize_for_transcription(&mut after);equal(gain,other);for(a,b)in before.iter().zip(&after){equal(*a,*b);}
  }
 }
 for mut before in [vec![0.0,-0.0],vec![f32::NAN,f32::INFINITY,f32::NEG_INFINITY,0.5],vec![f32::from_bits(1),-f32::from_bits(1)]] {
  let mut after=before.clone();equal(normalize_audio_for_transcription(&mut before),normalization_native::normalize_for_transcription(&mut after));for(a,b)in before.iter().zip(&after){equal(*a,*b);}
 }
}
