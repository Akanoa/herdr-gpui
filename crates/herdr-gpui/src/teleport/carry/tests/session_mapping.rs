use super::*;

#[test]
fn outside_home_and_traversal_are_refused() {
    for cwd in [
        "/outside",
        "/home/user-other",
        "/home/user/../other",
        "/home/user/notes/../../other",
    ] {
        let error = relative_cwd("/home/user", cwd).unwrap_err();
        assert!(matches!(error, Error::OutsideHome { .. }));
        assert!(error.to_string().contains("outside"));
    }
    assert_eq!(
        relative_cwd("/home/user/", "/home/user/notes").unwrap(),
        "notes"
    );
}

impl SessionReview {
    /// A ready session-only review for window tests; it never runs scripts.
    pub(crate) fn fixture(destination: Place) -> Self {
        Self {
            destination,
            cwd: "/home/there/notes".into(),
            source: SessionSource {
                cwd: "/home/here/notes".into(),
                relative: "notes".into(),
                work: Work::Session {
                    agent: AgentKind::Claude,
                    session: crate::teleport::snapshot::AgentSession {
                        agent: "claude".into(),
                        source: "herdr:claude".into(),
                        kind: crate::teleport::snapshot::SessionKind::Id,
                        value: "session-603".into(),
                    },
                    flags: Vec::new(),
                },
            },
        }
    }
}
