use super::super::syntax::qualified;
use crate::model::ParsedFile;
use sha2::{Digest, Sha256};
pub(super) fn overload(
    file: &ParsedFile,
    class: &str,
    name: &str,
    kind: &str,
    lang: &str,
    params: Option<&str>,
    test_file: bool,
) -> String {
    let id = qualified(class, name);
    if !file.syntax.declarations.contains_key(&id) {
        return id;
    }
    let parent = (!class.is_empty()).then_some(class);
    let payload = serde_json::to_string(&(
        kind,
        name,
        parent,
        lang,
        params,
        None::<&str>,
        None::<&str>,
        test_file || kind == "Test",
    ))
    .unwrap();
    let suffix = hex::encode(&Sha256::digest(payload)[..6]);
    let base = format!("{id}:S{suffix}");
    if !file.syntax.declarations.contains_key(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}:{n}"))
        .find(|v| !file.syntax.declarations.contains_key(v))
        .unwrap()
}
