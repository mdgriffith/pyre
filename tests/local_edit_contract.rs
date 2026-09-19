//! Bounded executable design model, deliberately independent of production code.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

type Rows = BTreeMap<String, Value>;

#[derive(Clone)]
struct Pending {
    id: String,
    ops: Vec<Value>,
    state: String,
    optimistic: bool,
    quarantined: bool,
    revision: Option<u64>,
}

struct Model {
    base: Rows,
    pending: Vec<Pending>,
    covered: u64,
    required: u64,
    target: Option<u64>,
    security_minimum: u64,
    fence: String,
    invalid: bool,
    failures: Vec<Value>,
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap()
}

// Stage on a copy so a missing target cannot leak an optimistic batch prefix.
fn apply(rows: &Rows, ops: &[Value]) -> Option<Rows> {
    let mut next = rows.clone();
    for op in ops {
        let key = text(op, "key");
        match text(op, "kind") {
            "create" => {
                if next.contains_key(key) {
                    return None;
                }
                next.insert(key.into(), op["row"].clone());
            }
            "update" => {
                let row = next.get_mut(key)?.as_object_mut()?;
                for setter in op["set"].as_array().unwrap() {
                    row.insert(setter[0].as_str().unwrap().into(), setter[1].clone());
                }
            }
            "delete" => {
                next.remove(key)?;
            }
            "command" => {}
            other => panic!("unknown operation {other}"),
        }
    }
    Some(next)
}

impl Model {
    fn visible(&self) -> Rows {
        if self.invalid {
            return Rows::new();
        }
        self.pending.iter().fold(self.base.clone(), |rows, p| {
            if p.optimistic
                && !p.quarantined
                && matches!(p.state.as_str(), "queued" | "sent" | "accepted")
            {
                apply(&rows, &p.ops).unwrap_or(rows)
            } else {
                rows
            }
        })
    }

    fn failure(&mut self, id: &str, certainty: &str) {
        self.failures.push(json!([id, certainty]));
    }

    fn step(&mut self, s: &Value) {
        let action = text(s, "action");
        if let Some(fence) = s.get("fence") {
            if action != "fence" && fence.as_str().unwrap() != self.fence {
                return;
            }
        }
        match action {
            "submit" => {
                let id = text(s, "id");
                if self.pending.iter().any(|p| p.id == id) {
                    return;
                }
                let ops = s["ops"].as_array().unwrap().clone();
                let invalid = ops
                    .iter()
                    .any(|op| op["kind"] == "update" && op["set"].as_array().unwrap().is_empty());
                let optimistic = ops
                    .iter()
                    .all(|op| op["safe"] == true && op["kind"] != "command")
                    && apply(&self.visible(), &ops).is_some();
                let state = if invalid {
                    self.failure(id, "rejected");
                    "rejected"
                } else if ops.is_empty() {
                    "confirmed"
                } else {
                    "queued"
                };
                self.pending.push(Pending {
                    id: id.into(),
                    ops,
                    state: state.into(),
                    optimistic,
                    quarantined: false,
                    revision: None,
                });
            }
            "dispatch" => {
                if self
                    .pending
                    .iter()
                    .any(|p| matches!(p.state.as_str(), "sent" | "unknown"))
                {
                    return;
                }
                if let Some(p) = self.pending.iter_mut().find(|p| p.state == "queued") {
                    assert_eq!(
                        p.id,
                        text(s, "id"),
                        "dispatch must preserve invocation order"
                    );
                    p.state = "sent".into();
                }
            }
            "accept" | "reject" | "unknown" => {
                let id = text(s, "id");
                let p = self.pending.iter_mut().find(|p| p.id == id).unwrap();
                if !matches!(p.state.as_str(), "sent" | "unknown") {
                    return;
                }
                match action {
                    "accept" => {
                        let revision = s["revision"].as_u64().unwrap();
                        p.revision = Some(revision);
                        // Definitive evidence either retires intent or proves a precommit base.
                        p.quarantined = false;
                        p.state = if !self.invalid && self.covered >= revision {
                            "confirmed"
                        } else {
                            "accepted"
                        }
                        .into();
                        self.required = self.required.max(revision);
                    }
                    "reject" => {
                        p.state = "rejected".into();
                        p.quarantined = false;
                        self.failure(id, "rejected");
                    }
                    _ => {
                        if p.state != "unknown" {
                            p.state = "unknown".into();
                            p.quarantined = true;
                            self.failure(id, "unknown");
                        }
                    }
                }
            }
            "partial" => {
                self.required = self.required.max(s["revision"].as_u64().unwrap());
            }
            "catchup" => {
                self.target = Some(self.required.max(self.covered).max(self.security_minimum));
            }
            "replace" => {
                let revision = s["revision"].as_u64().unwrap();
                let target = s["target"].as_u64().unwrap();
                if s["complete"] != true
                    || self.target != Some(target)
                    || revision < target
                    || revision < self.covered
                    || revision < self.security_minimum
                    || (revision == self.covered && !self.invalid)
                {
                    return;
                }
                self.base = serde_json::from_value(s["rows"].clone()).unwrap();
                self.covered = revision;
                self.invalid = false;
                for p in &mut self.pending {
                    if p.state == "accepted" && p.revision.unwrap() <= revision {
                        p.state = "confirmed".into();
                    } else if matches!(p.state.as_str(), "sent" | "unknown") {
                        p.quarantined = true;
                    }
                }
            }
            "invalidate" => {
                self.invalid = true;
                self.security_minimum = self.security_minimum.max(s["minimum"].as_u64().unwrap());
                self.required = self.required.max(self.security_minimum);
            }
            "fence" => {
                self.fence = text(s, "fence").into();
                self.base.clear();
                self.covered = 0;
                self.required = 0;
                self.target = None;
                self.security_minimum = 0;
                self.invalid = true;
                for p in &mut self.pending {
                    let certainty = match p.state.as_str() {
                        "queued" => "rejected",
                        "sent" | "unknown" => "unknown",
                        "accepted" => "acceptedUnreconciled",
                        _ => continue,
                    };
                    self.failures.push(json!([p.id, certainty]));
                }
                // Old-lifetime receipts are terminal; they cannot enter this lifetime's queue.
                self.pending.clear();
            }
            "expect" => {
                let states: Map<String, Value> = self
                    .pending
                    .iter()
                    .map(|p| (p.id.clone(), json!(p.state)))
                    .collect();
                let quarantined: Map<String, Value> = self
                    .pending
                    .iter()
                    .map(|p| (p.id.clone(), json!(p.quarantined)))
                    .collect();
                let actual = json!({"base": self.base, "visible": self.visible(),
                    "states": states, "covered": self.covered, "required": self.required,
                    "target": self.target, "securityMinimum": self.security_minimum,
                    "quarantined": quarantined,
                    "failures": self.failures, "invalid": self.invalid});
                for (key, expected) in s["value"].as_object().unwrap() {
                    assert!(actual.get(key).is_some(), "unknown expectation {key}");
                    assert_eq!(&actual[key], expected, "observation {key}");
                }
            }
            other => panic!("unknown transition {other}"),
        }
    }
}

#[test]
fn protocol_traces() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/local-edits/protocol.json")).unwrap();
    assert_eq!(fixtures["version"], 1);
    for trace in fixtures["traces"].as_array().unwrap() {
        eprintln!("trace: {}", trace["name"]);
        let mut model = Model {
            base: serde_json::from_value(trace["initial"].clone()).unwrap(),
            pending: vec![],
            covered: 0,
            required: 0,
            target: None,
            security_minimum: 0,
            fence: "main/tab1/auth1/e1".into(),
            invalid: false,
            failures: vec![],
        };
        for (index, step) in trace["steps"].as_array().unwrap().iter().enumerate() {
            eprintln!("  step {index}: {}", step["action"]);
            model.step(step);
        }
    }
}

#[test]
fn atomic_write_cardinality() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/local-edits/protocol.json")).unwrap();
    for case in fixtures["transactions"].as_array().unwrap() {
        let original: Rows = serde_json::from_value(case["initial"].clone()).unwrap();
        let mut staged = original.clone();
        let mut rejected = None;
        for (index, op) in case["ops"].as_array().unwrap().iter().enumerate() {
            // Actual target writes, never permission-filtered returned row count.
            if op["kind"] != "command" && op["actual"].as_u64().unwrap() != 1 {
                rejected = Some(index);
                break;
            }
            staged = apply(&staged, std::slice::from_ref(op)).unwrap();
        }
        let committed = rejected.is_none();
        let rows = if committed { staged } else { original };
        let revision = u64::from(committed && !case["ops"].as_array().unwrap().is_empty());
        assert_eq!(
            json!({"rows": rows, "rejectedIndex": rejected, "revision": revision}),
            case["expect"],
            "transaction {}",
            case["name"]
        );
    }
}
