use std::fs;

use humanize_plugin::flow::FlowLock;
use humanize_plugin::review::{ReviewDecision, ReviewStore};
use serde_json::{Value, json};

use super::driver_flows::{reviewed_lock_package, routed_locked_flow};
use super::support::DriverFixture;

#[test]
fn driver_rejects_missing_unknown_pending_rejected_and_forged_reviews() {
    let cases = ["missing", "unknown", "pending", "rejected", "forged"];
    for case in cases {
        let fixture = DriverFixture::new(&format!("driver-review-{case}"));
        let run_id = format!("run-review-{case}");
        let mut driver = fixture.spawn(&run_id);
        let package = reviewed_lock_package();
        let lock = serde_json::from_value::<FlowLock>(package.clone()).unwrap();
        let store = ReviewStore::new(fixture.root.join("reviews"));
        let review = store
            .prepare(
                &lock,
                &json!({"title":"Driver review gate"}),
                "<title>Driver review gate</title>\n",
            )
            .unwrap();
        if case == "rejected" {
            store
                .decide(
                    review.review_id(),
                    ReviewDecision::Rejected,
                    Some("operator rejected the flow"),
                )
                .unwrap();
        }

        let mut request = bind_request(&fixture, &run_id, package);
        match case {
            "missing" => {}
            "unknown" => request["review_id"] = json!("review_unknown"),
            "pending" | "rejected" => request["review_id"] = json!(review.review_id()),
            "forged" => {
                request["review_id"] = json!(review.review_id());
                request["review"] = json!({
                    "review_id": review.review_id(),
                    "status": "approved"
                });
            }
            _ => unreachable!(),
        }
        let response = raw_request(&fixture, request);

        assert_eq!(response["ok"], false, "{case}: {response}");
        let code = response["error"]["code"].as_str().unwrap();
        match case {
            "missing" => assert_eq!(code, "review_missing"),
            "unknown" => assert_eq!(code, "review_invalid"),
            "pending" | "forged" => assert_eq!(code, "review_pending"),
            "rejected" => assert_eq!(code, "review_rejected"),
            _ => unreachable!(),
        }
        driver.shutdown();
    }
}

#[test]
fn driver_rejects_review_binding_and_flow_lock_hash_mismatches() {
    let fixture = DriverFixture::new("driver-review-binding");
    let mut driver = fixture.spawn("run-review-binding");
    let reviewed_package = reviewed_lock_package();
    let reviewed_lock = serde_json::from_value::<FlowLock>(reviewed_package).unwrap();
    let store = ReviewStore::new(fixture.root.join("reviews"));
    let review = store
        .prepare(
            &reviewed_lock,
            &json!({"title":"Reviewed lock"}),
            "<title>Reviewed lock</title>\n",
        )
        .unwrap();
    let review = store
        .decide(review.review_id(), ReviewDecision::Approved, None)
        .unwrap();

    let mismatched = raw_request(
        &fixture,
        bind_request(&fixture, "run-review-binding", routed_locked_flow())
            .with_field("review_id", json!(review.review_id())),
    );
    assert_eq!(mismatched["ok"], false, "{mismatched}");
    assert_eq!(mismatched["error"]["code"], "review_binding_mismatch");

    let mut tampered = serde_json::to_value(&reviewed_lock).unwrap();
    tampered["content_hash"] = json!(format!("sha256:{}", "0".repeat(64)));
    let invalid = raw_request(
        &fixture,
        bind_request(&fixture, "run-review-binding", tampered)
            .with_field("review_id", json!(review.review_id())),
    );
    assert_eq!(invalid["ok"], false, "{invalid}");
    assert_eq!(invalid["error"]["code"], "invalid_flow_lock");
    driver.shutdown();
}

#[test]
fn driver_accepts_persisted_approved_and_bypassed_reviews() {
    for decision in [ReviewDecision::Approved, ReviewDecision::Bypassed] {
        let suffix = match decision {
            ReviewDecision::Approved => "approved",
            ReviewDecision::Bypassed => "bypassed",
            ReviewDecision::Rejected => unreachable!(),
        };
        let fixture = DriverFixture::new(&format!("driver-review-{suffix}"));
        let run_id = format!("run-review-{suffix}");
        let package = routed_locked_flow();
        let lock = serde_json::from_value::<FlowLock>(package.clone()).unwrap();
        let store = ReviewStore::new(fixture.root.join("reviews"));
        let review = store
            .prepare(
                &lock,
                &json!({"title":"Accepted lock"}),
                "<title>Accepted lock</title>\n",
            )
            .unwrap();
        let reason = (decision == ReviewDecision::Bypassed).then_some("operator bypass");
        let review = store.decide(review.review_id(), decision, reason).unwrap();
        drop(store);

        let mut driver = fixture.spawn(&run_id);
        let response = raw_request(
            &fixture,
            bind_request(&fixture, &run_id, package)
                .with_field("review_id", json!(review.review_id())),
        );

        assert_eq!(response["ok"], true, "{suffix}: {response}");
        assert_eq!(response["flow_lock_id"], lock.id());
        assert_eq!(response["content_hash"], lock.content_hash());
        driver.shutdown();
    }
}

#[test]
fn driver_rejects_mac_tampered_prepared_and_decision_authority() {
    for case in ["prepared", "decision", "forged"] {
        let fixture = DriverFixture::new(&format!("driver-review-mac-{case}"));
        let run_id = format!("run-review-mac-{case}");
        let package = reviewed_lock_package();
        let lock = serde_json::from_value::<FlowLock>(package.clone()).unwrap();
        let store = ReviewStore::new(fixture.root.join("reviews"));
        let review = store
            .prepare(
                &lock,
                &json!({"title":"MAC protected lock"}),
                "<title>MAC protected lock</title>\n",
            )
            .unwrap();

        match case {
            "prepared" => {
                let path = review.review_directory().join("prepared.json");
                let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value["record"]["format"] = json!("humanize.flow_review_prepared.v2");
                fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            }
            "decision" => {
                let rejected = store
                    .decide(
                        review.review_id(),
                        ReviewDecision::Rejected,
                        Some("operator rejected the flow"),
                    )
                    .unwrap();
                let path = rejected.decision_path().unwrap();
                let forged = fs::read_to_string(&path)
                    .unwrap()
                    .replace("rejected", "approved");
                fs::write(path, forged).unwrap();
            }
            "forged" => {
                let path = review.review_directory().join("prepared.json");
                let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value["mac"] = json!("0".repeat(64));
                fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            }
            _ => unreachable!(),
        }

        let mut driver = fixture.spawn(&run_id);
        let response = raw_request(
            &fixture,
            bind_request(&fixture, &run_id, package)
                .with_field("review_id", json!(review.review_id())),
        );
        assert_eq!(response["ok"], false, "{case}: {response}");
        assert_eq!(response["error"]["code"], "review_invalid");
        driver.shutdown();
    }
}

fn bind_request(fixture: &DriverFixture, run_id: &str, flow_lock: Value) -> Value {
    json!({
        "id": "bind-run",
        "token": fixture.token,
        "op": "bind_run",
        "run_id": run_id,
        "flow_lock": flow_lock
    })
}

fn raw_request(fixture: &DriverFixture, request: Value) -> Value {
    fixture.raw_request(&(request.to_string() + "\n"))
}

trait JsonField {
    fn with_field(self, key: &str, value: Value) -> Self;
}

impl JsonField for Value {
    fn with_field(mut self, key: &str, value: Value) -> Self {
        self.as_object_mut().unwrap().insert(key.to_string(), value);
        self
    }
}
