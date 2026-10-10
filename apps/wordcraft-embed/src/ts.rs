//! The embed API's TypeScript types, emitted into the generated `wordcraft_embed.d.ts` by
//! wasm-bindgen (declarations only; no JavaScript is written by hand). Kept outside the wasm-only
//! module so the test below runs natively.
//!
//! No comments inside the declarations: the test compares whitespace-collapsed text, and the
//! meaning of each field is in docs/embed.md.

pub const TS_DECLARATIONS: &str = r#"
export interface MountOptions {
    mode: 'view';
    theme: Theme;
}

export interface Theme {
    dark: boolean;
    accent: string;
    accentContent: string;
}

export type JumpTarget =
    | { kind: 'page'; page: number }
    | { kind: 'text'; text: string; page: number | null; occurrence: number };

export interface Mark {
    id: string;
    text: string;
    page: number | null;
    color: string;
}

export type EditorEvent =
    | { type: 'opened'; pages: number }
    | { type: 'openFailed'; message: string }
    | { type: 'pageChanged'; page: number }
    | { type: 'selection'; text: string; copyText: string; page: number | null; x: number; y: number }
    | { type: 'selectionCleared' }
    | { type: 'markActivated'; id: string }
    | { type: 'jumpNotFound'; target: JumpTarget };
"#;

#[cfg(test)]
mod tests {
    use super::TS_DECLARATIONS;

    fn squash(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The custom TypeScript section must declare the embed API's types exactly.
    #[test]
    fn declarations_match_the_embed_api() {
        let declared = squash(TS_DECLARATIONS);
        for decl in [
            "export interface MountOptions { mode: 'view'; theme: Theme; }",
            "export interface Theme { dark: boolean; accent: string; accentContent: string; }",
            "export type JumpTarget = | { kind: 'page'; page: number } | { kind: 'text'; text: string; page: number | null; occurrence: number };",
            "export interface Mark { id: string; text: string; page: number | null; color: string; }",
            "export type EditorEvent = | { type: 'opened'; pages: number } | { type: 'openFailed'; message: string } \
             | { type: 'pageChanged'; page: number } \
             | { type: 'selection'; text: string; copyText: string; page: number | null; x: number; y: number } \
             | { type: 'selectionCleared' } | { type: 'markActivated'; id: string } | { type: 'jumpNotFound'; target: JumpTarget };",
        ] {
            assert!(declared.contains(&squash(decl)), "missing or different: {decl}\nin: {declared}");
        }
    }
}
