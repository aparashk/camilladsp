use crate::filters::Filter;
use biquad;
use config;
use std::sync::{Arc, RwLock};

use NewValue;
use PrcFmt;
use ProcessingStatus;
use Res;

pub struct Loudness {
    pub name: String,
    ramptime_in_chunks: usize,
    current_volume: PrcFmt,
    target_volume: f32,
    ramp_start: PrcFmt,
    ramp_step: usize,
    samplerate: usize,
    chunksize: usize,
    processing_status: Arc<RwLock<ProcessingStatus>>,
    reference_level: f32,
    high_boost: f32,
    low_boost: f32,
    high_biquad: biquad::Biquad,
    low_biquad: biquad::Biquad,
}

fn get_rel_boost(level: f32, reference: f32) -> f32 {
    let mut rel_boost = (reference - level) / 20.0;
    if rel_boost < 0.0 {
        rel_boost = 0.0;
    } else if rel_boost > 1.0 {
        rel_boost = 1.0;
    }
    rel_boost
}

impl Loudness {
    pub fn from_config(
        name: String,
        conf: config::LoudnessParameters,
        chunksize: usize,
        samplerate: usize,
        processing_status: Arc<RwLock<ProcessingStatus>>,
    ) -> Self {
        let current_volume = processing_status.read().unwrap().volume;
        let ramptime_in_chunks =
            (conf.ramp_time / (1000.0 * chunksize as f32 / samplerate as f32)).round() as usize;
        let relboost = get_rel_boost(current_volume, conf.reference_level);
        let highshelf_conf = config::BiquadParameters::Highshelf {
            freq: 3500.0,
            slope: 12.0,
            gain: (relboost * conf.high_boost) as PrcFmt,
        };
        let lowshelf_conf = config::BiquadParameters::Lowshelf {
            freq: 70.0,
            slope: 12.0,
            gain: (relboost * conf.low_boost) as PrcFmt,
        };
        let high_biquad_coeffs =
            biquad::BiquadCoefficients::from_config(samplerate, highshelf_conf);
        let low_biquad_coeffs = biquad::BiquadCoefficients::from_config(samplerate, lowshelf_conf);
        let high_biquad =
            biquad::Biquad::new("highshelf".to_string(), samplerate, high_biquad_coeffs);
        let low_biquad = biquad::Biquad::new("lowshelf".to_string(), samplerate, low_biquad_coeffs);
        Loudness {
            name,
            ramptime_in_chunks,
            current_volume: current_volume as PrcFmt,
            ramp_start: current_volume as PrcFmt,
            target_volume: current_volume as f32,
            reference_level: conf.reference_level,
            high_boost: conf.high_boost,
            low_boost: conf.low_boost,
            high_biquad,
            low_biquad,
            ramp_step: 0,
            samplerate,
            chunksize,
            processing_status,
        }
    }

    fn make_ramp(&self) -> Vec<PrcFmt> {
        let ramprange =
            (self.target_volume as PrcFmt - self.ramp_start) / self.ramptime_in_chunks as PrcFmt;
        let stepsize = ramprange / self.chunksize as PrcFmt;
        (0..self.chunksize)
            .map(|val| {
                (PrcFmt::new(10.0)).powf(
                    (self.ramp_start
                        + ramprange * (self.ramp_step as PrcFmt - 1.0)
                        + val as PrcFmt * stepsize)
                        / 20.0,
                )
            })
            .collect()
    }
}

impl Filter for Loudness {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn process_waveform(&mut self, waveform: &mut Vec<PrcFmt>) -> Res<()> {
        let shared_vol = self.processing_status.read().unwrap().volume;

        // Volume setting changed
        if (shared_vol - self.target_volume).abs() > 0.001 {
            if self.ramptime_in_chunks > 0 {
                trace!("starting ramp {} -> {}", self.current_volume, shared_vol);
                self.ramp_start = self.current_volume;
                self.target_volume = shared_vol;
                self.ramp_step = 1;
            } else {
                self.current_volume = shared_vol as PrcFmt;
                self.target_volume = shared_vol;
                self.ramp_step = 0;
            }
        }
        // Not in a ramp
        if self.ramp_step == 0 {
            //debug!("constant gain {}", self.current_volume);
            let mut gain: PrcFmt = 10.0;
            gain = gain.powf(self.current_volume as PrcFmt / 20.0);
            for item in waveform.iter_mut() {
                *item *= gain;
            }
        }
        // Ramping
        else if self.ramp_step <= self.ramptime_in_chunks {
            trace!("ramp step {}", self.ramp_step);
            let ramp = self.make_ramp();
            self.ramp_step += 1;
            if self.ramp_step > self.ramptime_in_chunks {
                // Last step of ramp
                self.ramp_step = 0;
            }
            for (item, stepgain) in waveform.iter_mut().zip(ramp.iter()) {
                *item *= *stepgain;
            }
            self.current_volume = 20.0 * ramp.last().unwrap().log10();
            let relboost = get_rel_boost(self.current_volume as f32, self.reference_level);
            trace!(
                "Updating loudness biquads, relative boost {}%",
                100.0 * relboost
            );
            let highshelf_conf = config::BiquadParameters::Highshelf {
                freq: 3500.0,
                slope: 12.0,
                gain: (relboost * self.high_boost) as PrcFmt,
            };
            let lowshelf_conf = config::BiquadParameters::Lowshelf {
                freq: 70.0,
                slope: 12.0,
                gain: (relboost * self.low_boost) as PrcFmt,
            };
            self.high_biquad.update_parameters(config::Filter::Biquad {
                parameters: highshelf_conf,
            });
            self.low_biquad.update_parameters(config::Filter::Biquad {
                parameters: lowshelf_conf,
            });
        }
        if get_rel_boost(self.current_volume as f32, self.reference_level) > 0.0 {
            trace!("Applying loudness biquads");
            self.high_biquad.process_waveform(waveform).unwrap();
            self.low_biquad.process_waveform(waveform).unwrap();
        }
        Ok(())
    }

    fn update_parameters(&mut self, conf: config::Filter) {
        if let config::Filter::Loudness { parameters: conf } = conf {
            self.ramptime_in_chunks = (conf.ramp_time
                / (1000.0 * self.chunksize as f32 / self.samplerate as f32))
                .round() as usize;
            let current_volume = self.processing_status.read().unwrap().volume;
            let relboost = get_rel_boost(current_volume, conf.reference_level);
            let highshelf_conf = config::BiquadParameters::Highshelf {
                freq: 3500.0,
                slope: 12.0,
                gain: (relboost * conf.high_boost) as PrcFmt,
            };
            let lowshelf_conf = config::BiquadParameters::Lowshelf {
                freq: 70.0,
                slope: 12.0,
                gain: (relboost * conf.low_boost) as PrcFmt,
            };
            self.high_biquad.update_parameters(config::Filter::Biquad {
                parameters: highshelf_conf,
            });
            self.low_biquad.update_parameters(config::Filter::Biquad {
                parameters: lowshelf_conf,
            });
            self.reference_level = conf.reference_level;
            self.high_boost = conf.high_boost;
            self.low_boost = conf.low_boost;
        } else {
            // This should never happen unless there is a bug somewhere else
            panic!("Invalid config change!");
        }
    }
}
