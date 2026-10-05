//! Every text of the interface has its French translation. The interface is
//! written in English (`@tr("…")` in `ui/app-window.slint`); a text missing
//! from `translations/fr/LC_MESSAGES/resona.po` would show in English in a
//! French interface, silently. Checked by a test, so CI catches it.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    const UI: &str = include_str!("../ui/app-window.slint");
    const FRENCH: &str = include_str!("../translations/fr/LC_MESSAGES/resona.po");

    /// The string literal starting at `text` (just after its opening
    /// quote), unescaped as far as `\"` and `\\` go, which is all the
    /// interface uses; and the rest after the closing quote.
    fn literal(text: &str) -> Option<(String, &str)> {
        let mut out = String::new();
        let mut chars = text.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => out.push(chars.next()?.1),
                '"' => return Some((out, &text[i + 1..])),
                c => out.push(c),
            }
        }
        None
    }

    /// The texts after every occurrence of `marker` (which ends with `"`).
    fn texts_after(mut source: &str, marker: &str) -> BTreeSet<String> {
        let mut texts = BTreeSet::new();
        while let Some(at) = source.find(marker) {
            let Some((text, rest)) = literal(&source[at + marker.len()..]) else {
                break;
            };
            texts.insert(text);
            source = rest;
        }
        texts
    }

    #[test]
    fn literals_unescape() {
        assert_eq!(
            literal(r#"Use \"{}\" again"); x"#),
            Some(("Use \"{}\" again".to_owned(), "); x"))
        );
    }

    #[test]
    fn every_interface_text_is_translated_into_french() {
        let interface = texts_after(UI, "@tr(\"");
        // Every call is read: none written `@tr( "…")` slips through.
        assert_eq!(
            UI.matches("@tr(").count(),
            UI.matches("@tr(\"").count(),
            "an @tr( call not followed directly by a quote"
        );
        // Each entry is `msgid "…"` then `msgstr "…"`; an empty `msgstr`
        // shows the English text, so it does not count as translated.
        let ids: Vec<String> = texts_after(FRENCH, "\nmsgid \"").into_iter().collect();
        let mut translated = BTreeSet::new();
        let mut rest = FRENCH;
        while let Some(at) = rest.find("\nmsgid \"") {
            let (id, after_id) = literal(&rest[at + "\nmsgid \"".len()..]).expect("closed msgid");
            let at_str = after_id.find("msgstr \"").expect("msgid without msgstr");
            let (text, after) =
                literal(&after_id[at_str + "msgstr \"".len()..]).expect("closed msgstr");
            if !id.is_empty() {
                assert!(!text.is_empty(), "empty French translation for {id:?}");
                translated.insert(id);
            }
            rest = after;
        }
        assert_eq!(
            translated.len(),
            ids.iter().filter(|i| !i.is_empty()).count()
        );
        assert!(interface.len() > 50, "found only {} texts", interface.len());
        let missing: Vec<_> = interface.difference(&translated).collect();
        assert!(
            missing.is_empty(),
            "not translated into French: {missing:#?}"
        );
        let unused: Vec<_> = translated.difference(&interface).collect();
        assert!(unused.is_empty(), "translations no text uses: {unused:#?}");
    }
}
