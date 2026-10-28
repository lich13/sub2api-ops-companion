//! Language-model fingerprint detection (offline scoring and challenges).

mod analysis;
mod challenge;
mod codec;
mod data;
mod error;
mod features;
mod ranker;

use std::path::Path;

use serde::{Deserialize, Serialize};

pub use challenge::generate_challenges;
pub use data::Model;
pub use error::Error;
pub use features::parse_numbers;

/// One model reply and the number count its challenge asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    pub text: String,
    pub expected_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub id: String,
    pub expected_count: u64,
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Scored,
    Partial,
    Unscorable,
    NotModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbabilityStatus {
    Unavailable,
    ReferenceCalibrated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisResult {
    pub model: String,
    pub display_name: String,
    pub family: String,
    pub family_name: String,
    pub score: f64,
    pub probability: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub method: String,
    pub run: String,
    pub tau: f64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub index: usize,
    pub parsed_numbers: usize,
    pub minimum_numbers: u64,
    pub accepted: bool,
    pub raw_out_of_range_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub autocorrelation: f64,
    pub small_step_share: f64,
    pub model_like: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelVersion {
    pub base_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub insufficient: bool,
    pub label: String,
    pub reason: String,
    /// Always `None`; kept for the shared analysis shape.
    pub threshold: Option<f64>,
    pub method: String,
}

/// The scoring result. Every field is always present, so the shape matches the TS, Go, and Python SDKs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub decision: Decision,
    pub prediction: String,
    pub prediction_name: String,
    pub family_prediction: String,
    pub family_prediction_name: String,
    /// Candidates by ranking score, best first. Empty unless the decision is `Scored` or `Partial`.
    pub results: Vec<AnalysisResult>,
    pub ranking_score: Option<f64>,
    /// Closed-set probability of the top candidate within the reference bank.
    pub probability: Option<f64>,
    pub probability_status: ProbabilityStatus,
    pub probability_scope: Option<String>,
    pub calibration: Option<Calibration>,
    pub used_outputs: usize,
    pub diagnostics: Vec<Diagnostic>,
    pub sequence: Option<Sequence>,
    pub method: String,
    pub model_version: ModelVersion,
    pub evidence: Evidence,
}

/// Where [`Detector::load`] gets the reference data.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LoadOptions {
    /// A single local directory holding `unified_bank.json` and `shared_detector.bin`.
    pub sources: Vec<String>,
}

/// A loaded reference bank and detector.
pub struct Detector {
    data: data::ReferenceData,
}

impl Detector {
    /// Loads the reference data from a single local directory.
    pub fn load(options: &LoadOptions) -> Result<Self, Error> {
        Ok(Self {
            data: data::load(&options.sources)?,
        })
    }

    /// Loads the reference data directly from a local directory path.
    pub fn load_dir(dir: &Path) -> Result<Self, Error> {
        Ok(Self {
            data: data::load_dir(dir)?,
        })
    }

    /// Loads the reference data directly from in-memory `unified_bank.json` and `shared_detector.bin` bytes.
    pub fn load_bytes(bank_json: &[u8], detector_bin: &[u8]) -> Result<Self, Error> {
        Ok(Self {
            data: data::load_bytes(bank_json, detector_bin)?,
        })
    }

    /// `built_at` of the reference bank, which identifies the data build.
    pub fn built_at(&self) -> &str {
        &self.data.built_at
    }

    /// Parsed reference bank models in canonical detector order.
    pub fn models(&self) -> &[Model] {
        &self.data.models
    }

    /// Scores one to three saved replies. Three valid replies give a calibrated ranking; fewer give a partial
    /// ranking unless `strict` is set.
    pub fn analyze(&self, outputs: &[Output], strict: bool) -> Result<Analysis, Error> {
        analysis::analyze_outputs(outputs, &self.data, strict)
    }
}
