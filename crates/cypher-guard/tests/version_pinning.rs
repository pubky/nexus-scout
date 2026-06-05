//! Guards that the audited Neo4j version stays pinned across deployment configs
//! and CI; a silent image bump can outrun the sanitizer deny-list.

/// The audited Neo4j image. Bump only alongside a deny-list re-audit.
const AUDITED_NEO4J_IMAGE: &str = "neo4j:5.26-community";

#[test]
fn neo4j_image_tag_is_pinned_everywhere() {
    let files = [
        (
            "docker/docker-compose.yml",
            include_str!("../../../docker/docker-compose.yml"),
        ),
        (
            "docker/docker-compose.prod.yml",
            include_str!("../../../docker/docker-compose.prod.yml"),
        ),
        (
            ".github/workflows/test.yml",
            include_str!("../../../.github/workflows/test.yml"),
        ),
        (
            "docs/SECURITY_MATRIX.md",
            include_str!("../../../docs/SECURITY_MATRIX.md"),
        ),
    ];
    for (path, content) in files {
        assert!(
            content.contains(AUDITED_NEO4J_IMAGE),
            "{path} must pin {AUDITED_NEO4J_IMAGE}; a Neo4j version change requires \
             a deny-list re-audit"
        );
    }
}

/// The audited neo4rs driver version.
const AUDITED_NEO4RS: &str = "0.8.0";

#[test]
fn neo4rs_driver_version_is_pinned_consistently() {
    let cargo = include_str!("../../../Cargo.toml");
    let matrix = include_str!("../../../docs/SECURITY_MATRIX.md");
    assert!(
        cargo.contains(&format!("neo4rs = \"={AUDITED_NEO4RS}\"")),
        "workspace Cargo.toml must exact-pin neo4rs ={AUDITED_NEO4RS}"
    );
    assert!(
        matrix.contains(&format!("neo4rs ={AUDITED_NEO4RS}")),
        "docs/SECURITY_MATRIX.md must document the audited neo4rs {AUDITED_NEO4RS}"
    );
}
