//! Two declared models, which are not measurements and are never charted
//! beside one.
//!
//! What a remote object store would charge per open and per lookup is the
//! question the whole PMTiles argument is really about, and nothing in this
//! repository can measure it: there is no HTTP range reader, so there is no
//! round trip to time. What there is, from [`requests`](super::scenarios::requests),
//! is an exact count of requests and bytes, and a declared latency and
//! bandwidth turn that into a cost.
//!
//! That makes the result a model. Every value carries the parameters it was
//! computed from, by name and with units, so a reader can disagree with the
//! parameters instead of with the arithmetic, and
//! [`Modelled::CHARTABLE_BESIDE_MEASURED`] is `false` because a modelled
//! millisecond on the same axis as a measured one is a lie the axis tells for
//! you.

/// One named input to a model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parameter {
    pub name: &'static str,
    pub value: f64,
    pub unit: &'static str,
}

/// What a remote store would charge: a round trip per request, plus the bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RemoteModel {
    pub rtt_ms: f64,
    pub bandwidth_bytes_per_s: f64,
}

/// The round trip the declared model assumes.
pub const DECLARED_RTT_MS: f64 = 30.0;

/// The bandwidth the declared model assumes, in bytes a second.
pub const DECLARED_BANDWIDTH_BYTES_PER_S: f64 = 50.0 * 1024.0 * 1024.0;

impl RemoteModel {
    /// The model the suite publishes by default.
    pub fn declared() -> Self {
        Self {
            rtt_ms: DECLARED_RTT_MS,
            bandwidth_bytes_per_s: DECLARED_BANDWIDTH_BYTES_PER_S,
        }
    }

    /// `requests * rtt_ms + bytes / bandwidth`.
    pub fn cost_ms(&self, requests: u64, bytes: u64) -> f64 {
        requests as f64 * self.rtt_ms + (bytes as f64 / self.bandwidth_bytes_per_s) * 1000.0
    }

    /// The inputs, by name, so the page can print them beside the number.
    pub fn parameters(&self) -> Vec<Parameter> {
        vec![
            Parameter {
                name: "rtt_ms",
                value: self.rtt_ms,
                unit: "ms",
            },
            Parameter {
                name: "bandwidth_bytes_per_s",
                value: self.bandwidth_bytes_per_s,
                unit: "B/s",
            },
        ]
    }

    /// The modelled cost of one operation, carrying its own parameters.
    pub fn modelled(&self, name: &'static str, requests: u64, bytes: u64) -> Modelled {
        Modelled {
            name,
            value: self.cost_ms(requests, bytes),
            unit: "ms",
            parameters: self.parameters(),
        }
    }
}

/// What keeping a tree in sync would charge: a fixed cost per filesystem entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncModel {
    pub per_file_ms: f64,
}

/// The per-entry cost the declared model assumes.
pub const DECLARED_PER_FILE_MS: f64 = 2.0;

impl SyncModel {
    pub fn declared() -> Self {
        Self {
            per_file_ms: DECLARED_PER_FILE_MS,
        }
    }

    pub fn cost_ms(&self, filesystem_entries: u64) -> f64 {
        filesystem_entries as f64 * self.per_file_ms
    }

    pub fn parameters(&self) -> Vec<Parameter> {
        vec![Parameter {
            name: "per_file_ms",
            value: self.per_file_ms,
            unit: "ms",
        }]
    }

    pub fn modelled(&self, name: &'static str, filesystem_entries: u64) -> Modelled {
        Modelled {
            name,
            value: self.cost_ms(filesystem_entries),
            unit: "ms",
            parameters: self.parameters(),
        }
    }
}

/// A number that came out of a model rather than out of a machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Modelled {
    pub name: &'static str,
    pub value: f64,
    pub unit: &'static str,
    pub parameters: Vec<Parameter>,
}

impl Modelled {
    /// Whether a modelled value may share an axis with a measured one.
    ///
    /// It may not, and this is a constant rather than a comment so the renderer
    /// has something to read.
    pub const CHARTABLE_BESIDE_MEASURED: bool = false;

    /// Look one parameter up by name.
    pub fn parameter(&self, name: &str) -> Option<Parameter> {
        self.parameters
            .iter()
            .copied()
            .find(|parameter| parameter.name == name)
    }

    /// Every parameter this value names.
    pub fn parameter_names(&self) -> Vec<&'static str> {
        self.parameters
            .iter()
            .map(|parameter| parameter.name)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Reaching the document
// ---------------------------------------------------------------------------

use super::document::{Document, ModelledEntry};

/// The modelled rows a finished sweep publishes.
///
/// Built from the document's own invariants rather than from a scenario's
/// return value, so a model can only price something the sweep actually
/// measured: `remote_cost_ms` needs `requests` and `request_bytes`, which only
/// the `requests` scenario records, and `sync_cost_ms` needs
/// `filesystem_entries`, which only `generate` does. A cell missing its input
/// contributes no row rather than a row priced at zero.
pub fn entries_for(doc: &Document) -> Vec<ModelledEntry> {
    let remote = RemoteModel::declared();
    let sync = SyncModel::declared();
    let mut out: Vec<ModelledEntry> = Vec::new();
    let mut seen: Vec<(String, u32, &'static str)> = Vec::new();

    let params = |ps: Vec<Parameter>| {
        serde_json::Value::Object(
            ps.into_iter()
                .map(|p| {
                    (
                        p.name.to_string(),
                        serde_json::json!({ "value": p.value, "unit": p.unit }),
                    )
                })
                .collect(),
        )
    };

    for cell in &doc.cells {
        let inv = &cell.invariants;
        let key = |name: &'static str| (cell.backend.clone(), cell.scale, name);

        if let (Some(requests), Some(bytes)) = (inv.requests, inv.request_bytes)
            && !seen.contains(&key("remote_cost_ms"))
        {
            seen.push(key("remote_cost_ms"));
            out.push(ModelledEntry {
                library: cell.backend.clone(),
                scale: cell.scale,
                name: "remote_cost_ms".to_string(),
                value: remote.cost_ms(requests, bytes),
                unit: "ms".to_string(),
                model: params(remote.parameters()),
            });
        }

        if let Some(entries) = inv.filesystem_entries
            && !seen.contains(&key("sync_cost_ms"))
        {
            seen.push(key("sync_cost_ms"));
            out.push(ModelledEntry {
                library: cell.backend.clone(),
                scale: cell.scale,
                name: "sync_cost_ms".to_string(),
                value: sync.cost_ms(entries),
                unit: "ms".to_string(),
                model: params(sync.parameters()),
            });
        }
    }
    out
}
