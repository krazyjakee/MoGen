import "./style.css";
import init, { compile } from "../wasm/mogen_wasm.js";
import { createEditor } from "./editor";
import { createViewer } from "./viewer";

const editorEl = document.getElementById("editor")!;
const viewerEl = document.getElementById("viewer")!;
const statusEl = document.getElementById("status")!;
const diagnosticsEl = document.getElementById("diagnostics")!;
const examplePicker = document.getElementById("example-picker") as HTMLSelectElement;

// Examples are bundled at build time from `<repo>/examples/*.mog` so the page
// can ship without a server. `?raw` returns the file contents as a string;
// the glob is non-eager so we only load the .mog the user actually picks.
const exampleModules = import.meta.glob<string>("../../examples/*.mog", {
  query: "?raw",
  import: "default",
});

type Diag = {
  severity: "error" | "warning" | "note";
  code?: string;
  message: string;
  span?: { start: number; end: number } | null;
};

function setStatus(label: string, kind: "idle" | "compiling" | "ok" | "error") {
  statusEl.textContent = label;
  statusEl.className = `status ${kind}`;
}

function renderDiagnostics(diags: Diag[]) {
  diagnosticsEl.innerHTML = "";
  if (diags.length === 0) {
    diagnosticsEl.textContent = "no diagnostics";
    return;
  }
  for (const d of diags) {
    const span = document.createElement("span");
    span.className = `diag ${d.severity}`;
    const code = d.code ? `[${d.code}] ` : "";
    span.textContent = `${d.severity}: ${code}${d.message}`;
    diagnosticsEl.appendChild(span);
  }
}

function parseDiagnostics(text: string): Diag[] {
  const out: Diag[] = [];
  for (const line of text.split("\n")) {
    const t = line.trim();
    if (!t) continue;
    try {
      out.push(JSON.parse(t));
    } catch {
      // skip malformed lines
    }
  }
  return out;
}

async function bootstrap() {
  setStatus("loading wasm…", "compiling");
  await init();

  const viewer = createViewer(viewerEl);

  // Pick an interesting default that doesn't use CSG (the wasm build doesn't
  // ship the manifold C++ library, so `union`/`difference`/`intersect` are
  // refused with a diagnostic).
  const defaultExampleKey = Object.keys(exampleModules).find((k) => k.endsWith("/chair.mog"));
  const defaultSource = defaultExampleKey
    ? await exampleModules[defaultExampleKey]()
    : `scene {\n  box "hello" (size=[1, 1, 1])\n}\n`;

  // Populate the example picker. Strip the relative-path prefix so users see
  // bare filenames; keep the full key as the option value for lookup.
  const sortedKeys = Object.keys(exampleModules).sort();
  for (const key of sortedKeys) {
    const opt = document.createElement("option");
    opt.value = key;
    opt.textContent = key.replace(/^.*\//, "");
    examplePicker.appendChild(opt);
  }
  if (defaultExampleKey) examplePicker.value = defaultExampleKey;

  let compileTimer: number | null = null;
  let inflight = 0;

  async function runCompile(source: string) {
    const my = ++inflight;
    setStatus("compiling…", "compiling");
    try {
      // wasm-bindgen runs synchronously; yield to the event loop first so the
      // status text gets a chance to repaint.
      await new Promise((r) => setTimeout(r, 0));
      const outcome = compile(source);
      if (my !== inflight) {
        outcome.free();
        return;
      }
      const diags = parseDiagnostics(outcome.diagnostics);
      renderDiagnostics(diags);
      if (outcome.ok && outcome.glb) {
        await viewer.loadGlb(outcome.glb);
        setStatus(`ok (${diags.length} note${diags.length === 1 ? "" : "s"})`, "ok");
      } else {
        const errs = diags.filter((d) => d.severity === "error").length;
        setStatus(`${outcome.stage} failed (${errs} error${errs === 1 ? "" : "s"})`, "error");
      }
      outcome.free();
    } catch (e) {
      console.error(e);
      setStatus(`crash: ${(e as Error).message}`, "error");
    }
  }

  function scheduleCompile(source: string) {
    if (compileTimer !== null) clearTimeout(compileTimer);
    compileTimer = window.setTimeout(() => runCompile(source), 350);
  }

  const editor = createEditor(editorEl, defaultSource, scheduleCompile);

  examplePicker.addEventListener("change", async () => {
    const key = examplePicker.value;
    if (!key || !(key in exampleModules)) return;
    const source = await exampleModules[key]();
    editor.setSource(source);
    runCompile(source);
  });

  await runCompile(defaultSource);
}

bootstrap().catch((e) => {
  console.error(e);
  setStatus(`init failed: ${(e as Error).message}`, "error");
});
