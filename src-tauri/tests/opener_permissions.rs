use serde_json::Value;

fn permission_identifiers(source: &str) -> Vec<String> {
    let capability: Value = serde_json::from_str(source).unwrap();
    capability["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|permission| {
            permission
                .as_str()
                .or_else(|| permission["identifier"].as_str())
                .map(str::to_owned)
        })
        .collect()
}

#[test]
fn native_capabilities_allow_opening_web_urls() {
    for (name, source) in [
        ("desktop", include_str!("../capabilities/desktop.json")),
        ("android", include_str!("../capabilities/mobile.json")),
        ("ios", include_str!("../capabilities/ios.json")),
    ] {
        let permissions = permission_identifiers(source);
        for required in ["opener:allow-open-url", "opener:allow-default-urls"] {
            assert!(
                permissions.iter().any(|permission| permission == required),
                "{name} capability is missing {required}"
            );
        }
    }
}
