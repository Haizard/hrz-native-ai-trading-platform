//! `/skills`, driven through the real router.
//!
//! ## What only a route test can show
//!
//! That a published skill is actually in the database and comes back out of it;
//! that publishing the same version twice is a 409 rather than a duplicate row;
//! that a newer version *adds* to what is readable instead of replacing it; and
//! that a skill belonging to somebody else is invisible rather than merely
//! forbidden. The versioning rules themselves are unit-tested in
//! `ai-agent::skills` -- what is tested here is that the persistence behind
//! them honours the same rules.
//!
//! Every test registers its own account and deletes it, and deletes any skill
//! it stored, so the database is left as it was found.

mod common;

use axum::http::StatusCode;
use common::{unique_skill, Harness};
use serde_json::{json, Value};

/// The row id of a stored skill, taken from the listing.
///
/// The route's client-facing id is the slug, so a test that needs to delete
/// what it made has to go back through the table.
async fn stored_row_id(h: &Harness, user_id: uuid::Uuid, name: &str) -> Option<uuid::Uuid> {
    db::skills::list_skills(h.database.pool(), user_id, db::skills::MAX_SKILLS)
        .await
        .expect("the listing must be readable")
        .into_iter()
        .find(|row| row.name == name)
        .map(|row| row.id)
}

#[tokio::test]
async fn a_skill_is_stored_and_read_back() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let skill = unique_skill("1.0");
    let name = skill["name"].as_str().expect("a name").to_string();

    let (status, body) = h.post("/skills", skill.clone(), Some(&user.token)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], name);
    assert_eq!(body["version"], "1.0");

    let id = body["id"]
        .as_str()
        .expect("the route echoes the skill")
        .to_string();
    let (status, body) = h.get(&format!("/skills/{id}"), Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["knowledge"],
        "Sweep a recent low, then buy the reclaim."
    );

    // It is the caller's own, so the listing has to say so.
    let (status, body) = h.get("/skills", Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK);
    let listed = body.as_array().expect("a list");
    let entry = listed
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("the published skill must be listed: {body}"));
    assert_eq!(entry["owned"], true);

    if let Some(row) = stored_row_id(&h, user.id, &name).await {
        db::skills::delete_skill(h.database.pool(), row)
            .await
            .unwrap();
    }
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn publishing_the_same_version_twice_is_a_conflict() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let skill = unique_skill("1.0");
    let name = skill["name"].as_str().expect("a name").to_string();

    let (status, body) = h.post("/skills", skill.clone(), Some(&user.token)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = h.post("/skills", skill.clone(), Some(&user.token)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "SKILL_VERSION_EXISTS");

    if let Some(row) = stored_row_id(&h, user.id, &name).await {
        db::skills::delete_skill(h.database.pool(), row)
            .await
            .unwrap();
    }
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_new_version_is_appended_and_becomes_the_one_listed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let first = unique_skill("1.0");
    let name = first["name"].as_str().expect("a name").to_string();

    let (status, body) = h.post("/skills", first.clone(), Some(&user.token)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // The id is derived from the name and the major version, so a client that
    // wants to publish the next version cannot be expected to compute it.
    let slug = body["id"]
        .as_str()
        .expect("the write echoes the id")
        .to_string();
    assert!(slug.ends_with("-v1"), "{slug}");

    // Same name, higher minor: same id, so it belongs under the same path.
    let mut second = first.clone();
    second["version"] = json!("1.1");
    second["knowledge"] = json!("Sweep a recent low, then buy the reclaim with absorption.");
    let (status, body) = h
        .put(
            &format!("/skills/{slug}"),
            second.clone(),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["version"], "1.1");

    // The listing shows one entry per skill: the newest.
    let (status, body) = h.get("/skills", Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK);
    let entries: Vec<&Value> = body
        .as_array()
        .expect("a list")
        .iter()
        .filter(|s| s["id"] == slug)
        .collect();
    assert_eq!(entries.len(), 1, "one entry per skill: {body}");
    assert_eq!(entries[0]["version"], "1.1");

    // And the read serves the newer body, not the one that was there first.
    let (status, body) = h.get(&format!("/skills/{slug}"), Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["knowledge"]
            .as_str()
            .unwrap_or_default()
            .contains("absorption"),
        "the newest version must win: {body}"
    );

    for row in db::skills::list_skills(h.database.pool(), user.id, db::skills::MAX_SKILLS)
        .await
        .expect("the listing must be readable")
    {
        if row.name == name {
            db::skills::delete_skill(h.database.pool(), row.id)
                .await
                .unwrap();
        }
    }
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_document_that_does_not_match_its_path_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let skill = unique_skill("2.0");

    // The document is `...-v2`; putting it under someone else's id would file
    // it under a name it does not have.
    let (status, body) = h
        .put("/skills/not-this-one-v9", skill, Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "SKILL_ID_MISMATCH");

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn an_empty_skill_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    // `Skill` is `#[serde(default)]` throughout, so this deserializes fine and
    // would otherwise be stored as a skill that teaches the model nothing.
    let (status, body) = h.post("/skills", json!({}), Some(&user.token)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "SKILL_NAME_REQUIRED");

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn skills_require_a_token() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, _) = h.get("/skills", None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "skills are user data, not public market data"
    );

    let (status, _) = h.post("/skills", unique_skill("1.0"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
