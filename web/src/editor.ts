import { EditorState } from "@codemirror/state";
import { EditorView, lineNumbers, highlightActiveLineGutter, keymap } from "@codemirror/view";
import {
  bracketMatching,
  defaultHighlightStyle,
  foldGutter,
  syntaxHighlighting,
  StreamLanguage,
} from "@codemirror/language";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { tags as t } from "@lezer/highlight";
import { HighlightStyle } from "@codemirror/language";

// Minimal .mog tokenizer. Doesn't try to be the pest grammar — just enough to
// colour comments, strings, numbers, attribute keys, and the small set of
// well-known node kinds. Anything else falls through as a plain identifier.
const KINDS = new Set([
  "scene", "group", "module", "use", "material", "joint", "clip", "track",
  "skin", "bone", "connector", "attach", "mirror", "array", "stack", "grid",
  "union", "difference", "intersect", "lod_scale",
  "box", "rounded_box", "cylinder", "cone", "sphere", "capsule", "torus",
  "prism", "pyramid", "disc", "icosphere", "ellipsoid", "frustum", "wedge",
  "plane", "quad", "tube", "lathe", "spline_tube", "leaf_card", "curved_plane",
  "half_cylinder", "hemisphere", "superellipsoid", "torus_arc",
  "spin", "open_close", "wave", "flap", "idle",
]);

const mogLanguage = StreamLanguage.define({
  name: "mog",
  startState: () => ({ inAttrs: 0 }),
  token(stream, state) {
    if (stream.eatSpace()) return null;
    if (stream.match("//")) {
      stream.skipToEnd();
      return "comment";
    }
    if (stream.match(/^"(?:\\.|[^"\\])*"/)) return "string";
    if (stream.match(/^-?\d+(?:\.\d+)?/)) return "number";

    const ch = stream.peek();
    if (ch === "(") { stream.next(); state.inAttrs++; return "punctuation"; }
    if (ch === ")") { stream.next(); if (state.inAttrs > 0) state.inAttrs--; return "punctuation"; }
    if (ch === "{" || ch === "}" || ch === "[" || ch === "]" || ch === "," || ch === ";") {
      stream.next();
      return "punctuation";
    }
    if (ch === "$") {
      stream.next();
      stream.eatWhile(/[A-Za-z0-9_]/);
      return "variableName";
    }
    if (stream.match(/^[A-Za-z_][A-Za-z0-9_]*/)) {
      const word = stream.current();
      // Inside `(…)` an identifier followed by `=` is an attribute key.
      if (state.inAttrs > 0) {
        const rest = stream.string.slice(stream.pos);
        if (/^\s*=/.test(rest)) return "propertyName";
      }
      if (KINDS.has(word)) return "keyword";
      return "variableName";
    }
    stream.next();
    return null;
  },
  tokenTable: {},
});

const mogHighlight = HighlightStyle.define([
  { tag: t.comment, color: "#7a7e87", fontStyle: "italic" },
  { tag: t.string, color: "#b0d684" },
  { tag: t.number, color: "#f0a070" },
  { tag: t.keyword, color: "#6aa9ff", fontWeight: "600" },
  { tag: t.propertyName, color: "#e5a8ff" },
  { tag: t.variableName, color: "#d8dade" },
  { tag: t.punctuation, color: "#8a8f99" },
]);

export type EditorHandle = {
  view: EditorView;
  setSource: (text: string) => void;
  getSource: () => string;
};

export function createEditor(
  parent: HTMLElement,
  initial: string,
  onChange: (source: string) => void,
): EditorHandle {
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc: initial,
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        foldGutter(),
        history(),
        bracketMatching(),
        syntaxHighlighting(defaultHighlightStyle),
        syntaxHighlighting(mogHighlight),
        mogLanguage,
        keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
        EditorView.theme({
          "&": { backgroundColor: "transparent", color: "var(--text)" },
          ".cm-gutters": {
            backgroundColor: "transparent",
            color: "var(--muted)",
            border: "none",
          },
          ".cm-activeLine": { backgroundColor: "rgba(106,169,255,0.06)" },
          ".cm-activeLineGutter": { backgroundColor: "rgba(106,169,255,0.06)" },
          ".cm-cursor": { borderLeftColor: "var(--accent)" },
          ".cm-selectionBackground, ::selection": {
            backgroundColor: "rgba(106,169,255,0.25) !important",
          },
        }, { dark: true }),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) onChange(u.state.doc.toString());
        }),
      ],
    }),
  });

  return {
    view,
    setSource(text) {
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: text },
      });
    },
    getSource() {
      return view.state.doc.toString();
    },
  };
}
