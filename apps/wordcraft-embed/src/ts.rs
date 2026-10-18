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
    viewBar?: boolean;
}

export interface Theme {
    dark: boolean;
    accent: string;
    accentContent: string;
}

export type JumpTarget =
    | { kind: 'page'; page: number }
    | { kind: 'text'; text: string; page: number | null; occurrence: number }
    | { kind: 'location'; at: LocationSpec };

export interface Mark {
    id: string;
    text: string;
    page: number | null;
    color: string;
}

export interface LocationSpec {
    block: BlockRef;
    fingerprint: { length: number; hash: string };
    from?: number | null;
    to?: number | null;
}

export type EditorMark = { id: string; color: string; style?: 'fill' | 'outline' | 'bar'; page?: number | null } & ({ text: string } | { at: LocationSpec });

export interface EditorAuthor {
    id: string;
    name: string;
}

export interface EditorPermissions {
    acceptRejectChanges: boolean;
    organizePages: boolean;
    fillForms: boolean;
    redact: boolean;
    stamps: boolean;
    drawing: boolean;
    fullEditing?: boolean;
}

export type ModeRequest =
    | { mode: 'view' }
    | { mode: 'edit'; author: EditorAuthor; revisionDate: string; permissions: EditorPermissions };

export interface OpenOptions {
    display?: 'asStored' | 'final';
}

export type ZoomRequest = { zoom: number | 'pageWidth'; token: number };

export interface BlockRef {
    part: string;
    index: number;
}

export type ScrollRequest = { block: BlockRef; fraction: number; token: number };

export type LocateRequest = { blocks: BlockRef[]; token: number };

export type FindRequest = { text: string; matchCase?: boolean; step: 'first' | 'next' | 'previous'; select?: boolean; token: number };

export type EditorEvent =
    | { type: 'opened'; pages: number }
    | { type: 'openFailed'; message: string }
    | { type: 'pageChanged'; page: number }
    | { type: 'selection'; text: string; copyText: string; page: number | null; x: number; y: number }
    | { type: 'selectionCleared' }
    | { type: 'markActivated'; id: string }
    | { type: 'jumpNotFound'; target: JumpTarget }
    | { type: 'modeChanged'; mode: 'view' | 'edit' }
    | { type: 'dirtyChanged'; dirty: boolean }
    | { type: 'saveRequested'; bytes: Uint8Array; fileName: string }
    | { type: 'saveFailed'; message: string }
    | { type: 'commentRequested'; text: string; copyText: string; page: number | null; x: number; y: number }
    | { type: 'findRequested' }
    | { type: 'zoomChanged'; percent: number; pageWidth: boolean; cause: 'user' | 'host'; token: number | null }
    | { type: 'viewport'; top: BlockRef | null; fraction: number; visibleHeight: number; page: number; pages: number; cause: 'user' | 'host'; token: number | null }
    | { type: 'blockNotFound'; block: BlockRef; token: number }
    | { type: 'located'; positions: (number | null)[]; height: number; token: number }
    | { type: 'findResult'; count: number; index: number | null; token: number }
    | { type: 'locationMismatch'; ids: string[] }
    | { type: 'marksDropped'; count: number }
    | { type: 'redactionsPending'; marks: number }
    | { type: 'editRefused'; code: EditRefusalCode };

export type EditRefusalCode =
    | 'command_not_allowed'
    | 'decision_not_allowed'
    | 'paragraph_break_not_supported'
    | 'character_not_allowed'
    | 'outside_editable_story'
    | 'invalid_edit_session'
    | 'unknown_command'
    | 'invalid_parameters'
    | 'not_available'
    | 'document_protected'
    | 'command_failed';
"#;

#[cfg(test)]
mod tests {
    use super::TS_DECLARATIONS;

    fn squash(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The custom TypeScript section must declare the embed API's types exactly, and `web.rs` must
    /// declare each of `EmbeddedEditor`'s methods with the API's name and argument types (the
    /// package script checks the generated `.d.ts` for the same signatures).
    #[test]
    fn declarations_match_the_embed_api() {
        let declared = squash(TS_DECLARATIONS);
        for decl in [
            "export interface MountOptions { mode: 'view'; theme: Theme; viewBar?: boolean; }",
            "export interface Theme { dark: boolean; accent: string; accentContent: string; }",
            "export type JumpTarget = | { kind: 'page'; page: number } | { kind: 'text'; text: string; page: number | null; occurrence: number } | { kind: 'location'; at: LocationSpec };",
            "export interface Mark { id: string; text: string; page: number | null; color: string; }",
            "export interface LocationSpec { block: BlockRef; fingerprint: { length: number; hash: string }; from?: number | null; to?: number | null; }",
            "export type EditorMark = { id: string; color: string; style?: 'fill' | 'outline' | 'bar'; page?: number | null } & ({ text: string } | { at: LocationSpec });",
            "export interface EditorAuthor { id: string; name: string; }",
            "export interface EditorPermissions { acceptRejectChanges: boolean; organizePages: boolean; fillForms: boolean; redact: boolean; stamps: boolean; drawing: boolean; fullEditing?: boolean; }",
            "export type ModeRequest = | { mode: 'view' } | { mode: 'edit'; author: EditorAuthor; revisionDate: string; permissions: EditorPermissions };",
            "export interface OpenOptions { display?: 'asStored' | 'final'; }",
            "export type ZoomRequest = { zoom: number | 'pageWidth'; token: number };",
            "export interface BlockRef { part: string; index: number; }",
            "export type ScrollRequest = { block: BlockRef; fraction: number; token: number };",
            "export type LocateRequest = { blocks: BlockRef[]; token: number };",
            "export type FindRequest = { text: string; matchCase?: boolean; step: 'first' | 'next' | 'previous'; select?: boolean; token: number };",
            "export type EditorEvent = | { type: 'opened'; pages: number } | { type: 'openFailed'; message: string } \
             | { type: 'pageChanged'; page: number } \
             | { type: 'selection'; text: string; copyText: string; page: number | null; x: number; y: number } \
             | { type: 'selectionCleared' } | { type: 'markActivated'; id: string } | { type: 'jumpNotFound'; target: JumpTarget } \
             | { type: 'modeChanged'; mode: 'view' | 'edit' } | { type: 'dirtyChanged'; dirty: boolean } \
             | { type: 'saveRequested'; bytes: Uint8Array; fileName: string } | { type: 'saveFailed'; message: string } \
             | { type: 'commentRequested'; text: string; copyText: string; page: number | null; x: number; y: number } \
             | { type: 'findRequested' } \
             | { type: 'zoomChanged'; percent: number; pageWidth: boolean; cause: 'user' | 'host'; token: number | null } \
             | { type: 'viewport'; top: BlockRef | null; fraction: number; visibleHeight: number; page: number; pages: number; cause: 'user' | 'host'; token: number | null } \
             | { type: 'blockNotFound'; block: BlockRef; token: number } \
             | { type: 'located'; positions: (number | null)[]; height: number; token: number } \
             | { type: 'findResult'; count: number; index: number | null; token: number } \
             | { type: 'locationMismatch'; ids: string[] } \
             | { type: 'marksDropped'; count: number } \
             | { type: 'redactionsPending'; marks: number } | { type: 'editRefused'; code: EditRefusalCode };",
            "export type EditRefusalCode = | 'command_not_allowed' | 'decision_not_allowed' | 'paragraph_break_not_supported' \
             | 'character_not_allowed' | 'outside_editable_story' | 'invalid_edit_session' \
             | 'unknown_command' | 'invalid_parameters' | 'not_available' | 'document_protected' \
             | 'command_failed';",
        ] {
            assert!(declared.contains(&squash(decl)), "missing or different: {decl}\nin: {declared}");
        }

        // The methods, as `web.rs` declares them to wasm-bindgen (the release glue checks no types,
        // so the names and argument types are what the `.d.ts` promises).
        let web = squash(include_str!("web.rs"));
        for (method, part) in [
            ("mount", "#[wasm_bindgen(unchecked_return_type = \"Promise<EmbeddedEditor>\")] pub fn mount("),
            ("mount", "#[wasm_bindgen(unchecked_param_type = \"MountOptions\")] options: JsValue, ) -> js_sys::Promise"),
            (
                "open",
                "pub fn open( &self, #[wasm_bindgen(unchecked_param_type = \"Uint8Array\")] bytes: JsValue, \
                 #[wasm_bindgen(js_name = \"fileName\", unchecked_param_type = \"string\")] file_name: JsValue, \
                 options: Option<OpenOptionsArgument>, ) -> Result<(), JsValue>",
            ),
            (
                "jumpTo",
                "#[wasm_bindgen(js_name = jumpTo)] pub fn jump_to(&self, #[wasm_bindgen(unchecked_param_type = \"JumpTarget\")] target: JsValue)",
            ),
            (
                "setMarks",
                "#[wasm_bindgen(js_name = setMarks)] pub fn set_marks(&self, #[wasm_bindgen(unchecked_param_type = \"EditorMark[]\")] marks: JsValue)",
            ),
            (
                "setTheme",
                "#[wasm_bindgen(js_name = setTheme)] pub fn set_theme(&self, #[wasm_bindgen(unchecked_param_type = \"Theme\")] theme: JsValue)",
            ),
            (
                "setListener",
                "#[wasm_bindgen(js_name = setListener)] pub fn set_listener(&self, #[wasm_bindgen(unchecked_param_type = \"(event: EditorEvent) => void\")] listener: JsValue)",
            ),
            ("destroy", "pub fn destroy(&self) {"),
            (
                "setMode",
                "#[wasm_bindgen(js_name = setMode)] pub fn set_mode(&self, #[wasm_bindgen(unchecked_param_type = \"ModeRequest\")] request: JsValue)",
            ),
            (
                "setZoom",
                "#[wasm_bindgen(js_name = setZoom)] pub fn set_zoom(&self, #[wasm_bindgen(unchecked_param_type = \"ZoomRequest\")] request: JsValue)",
            ),
            (
                "scrollToBlock",
                "#[wasm_bindgen(js_name = scrollToBlock)] pub fn scroll_to_block(&self, #[wasm_bindgen(unchecked_param_type = \"ScrollRequest\")] request: JsValue)",
            ),
            (
                "locate",
                "#[wasm_bindgen(js_name = locate)] pub fn locate(&self, #[wasm_bindgen(unchecked_param_type = \"LocateRequest\")] request: JsValue)",
            ),
            ("find", "#[wasm_bindgen(js_name = find)] pub fn find(&self, #[wasm_bindgen(unchecked_param_type = \"FindRequest\")] request: JsValue)"),
            ("requestSave", "#[wasm_bindgen(js_name = requestSave)] pub fn request_save(&self) {"),
            ("applyRedactions", "#[wasm_bindgen(js_name = applyRedactions)] pub fn apply_redactions(&self) {"),
        ] {
            assert!(web.contains(&squash(part)), "{method}: web.rs lacks {part}");
        }
    }

    /// `editRefused` names exactly the API's eleven codes, in its order (the host bridge's side is
    /// `events_serialise_to_the_embed_api_shapes` in `wordcraft-ui-egui`).
    #[test]
    fn the_refusal_codes_are_the_api_s() {
        let declared = squash(TS_DECLARATIONS);
        let union = declared.split("export type EditRefusalCode =").nth(1).and_then(|rest| rest.split(';').next()).unwrap_or_default();
        let codes: Vec<&str> = union.split('|').map(|code| code.trim().trim_matches('\'')).filter(|code| !code.is_empty()).collect();
        assert_eq!(
            codes,
            [
                "command_not_allowed",
                "decision_not_allowed",
                "paragraph_break_not_supported",
                "character_not_allowed",
                "outside_editable_story",
                "invalid_edit_session",
                "unknown_command",
                "invalid_parameters",
                "not_available",
                "document_protected",
                "command_failed",
            ]
        );
    }
}
