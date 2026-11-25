use super::qualified;
use crate::model::ParsedFile;

pub(super) fn calls(file: &mut ParsedFile, cpp: bool) {
    use std::collections::HashMap;
    let mut definitions = HashMap::<&str, Vec<(&str, Option<&str>, bool)>>::new();
    let mut owners = HashMap::new();
    for (d, s) in file.cfamily.declarations.iter().zip(&file.symbols) {
        let items = definitions.entry(&d.name).or_default();
        let entry = (d.identity.as_str(), d.parent.as_deref(), s.kind != "Class");
        if !items.contains(&entry) {
            items.push(entry);
        }
        owners.insert(d.identity.as_str(), d.parent.as_deref().unwrap_or(""));
    }
    for site in file.cfamily.sites.values_mut().flatten() {
        let raw = if let Some(receiver) = &site.receiver {
            site.raw
                .strip_prefix(&format!("{receiver}."))
                .unwrap_or(&site.raw)
        } else {
            &site.raw
        };
        let (explicit, name) = raw
            .rsplit_once("::")
            .map_or((None, raw), |(scope, name)| (Some(scope), name));
        let options = definitions.get(name).map(Vec::as_slice).unwrap_or_default();
        let mut items: Vec<_> = options
            .iter()
            .filter(|(_, _, callable)| !cpp || *callable || !options.iter().any(|e| e.2))
            .collect();
        if !cpp {
            site.target = items.first().map(|item| item.0.to_owned());
            continue;
        }
        if site.receiver.as_ref().is_some_and(|r| r != "this") {
            site.unresolved = true;
            site.candidates = items.iter().map(|e| e.0.to_owned()).collect();
            continue;
        }
        let mut scope = if raw.starts_with("::") {
            ""
        } else {
            owners.get(site.caller.as_str()).copied().unwrap_or("")
        };
        let qual = explicit.map(|s| s.trim_start_matches("::").replace("::", "."));
        let all = items.clone();
        loop {
            let preferred = match &qual {
                Some(q) if !q.is_empty() => qualified(scope, q),
                _ => scope.into(),
            };
            items = all
                .iter()
                .copied()
                .filter(|e| e.1.unwrap_or("") == preferred)
                .collect();
            if !items.is_empty() || scope.is_empty() {
                break;
            }
            scope = scope.rsplit_once('.').map_or("", |(s, _)| s);
        }
        match items.as_slice() {
            [only] => site.target = Some(only.0.to_owned()),
            [] if explicit.is_none() && !all.is_empty() => {
                site.unresolved = true;
                site.candidates = all.iter().map(|e| e.0.to_owned()).collect();
            }
            many if many.len() > 1 => {
                site.ambiguous = true;
                site.candidates = many.iter().map(|e| e.0.to_owned()).collect();
            }
            _ => {}
        }
    }
}
