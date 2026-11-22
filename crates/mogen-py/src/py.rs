//! `mogen._native`: the PyO3 module behind the pure-Python `mogen` package.
//!
//! The boundary is deliberately small and data-only: syntax trees cross it as
//! plain dicts (see `node_to_py` for the shape) and the Python layer owns the
//! object model. Formatting snapshots (`orig`) travel as opaque tokens so
//! Python can't corrupt them.

use std::path::PathBuf;

use mogen_core::{Diagnostic, Severity};
use mogen_dsl::syntax::{self, AttrOrig, NodeOrig, SyntaxAttr, SyntaxDoc, SyntaxNode};
use mogen_dsl::Value;
use pyo3::exceptions::{PyIOError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};

use crate::Format;

#[pyclass(frozen, module = "mogen._native")]
struct NodeToken(NodeOrig);

#[pyclass(frozen, module = "mogen._native")]
struct AttrToken(AttrOrig);

fn value_err(e: anyhow::Error) -> PyErr {
    PyValueError::new_err(format!("{e:#}"))
}

// --- syntax tree <-> dicts --------------------------------------------------

/// `{"kind", "name", "attrs": [{"key", "value", "leading", "trailing",
/// "orig"}], "children": list | None, "leading", "orig"}`
fn node_to_py<'py>(py: Python<'py>, n: &SyntaxNode) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("kind", &n.kind)?;
    d.set_item("name", &n.name)?;
    let attrs = PyList::empty(py);
    for a in &n.attrs {
        let ad = PyDict::new(py);
        ad.set_item("key", &a.key)?;
        ad.set_item("value", &a.value)?;
        ad.set_item("leading", &a.leading)?;
        ad.set_item("trailing", &a.trailing)?;
        match &a.orig {
            Some(o) => ad.set_item("orig", Py::new(py, AttrToken(o.clone()))?)?,
            None => ad.set_item("orig", py.None())?,
        }
        attrs.append(ad)?;
    }
    d.set_item("attrs", attrs)?;
    match &n.children {
        Some(kids) => {
            let list = PyList::empty(py);
            for k in kids {
                list.append(node_to_py(py, k)?)?;
            }
            d.set_item("children", list)?;
        }
        None => d.set_item("children", py.None())?,
    }
    d.set_item("leading", &n.leading)?;
    match &n.orig {
        Some(o) => d.set_item("orig", Py::new(py, NodeToken(o.clone()))?)?,
        None => d.set_item("orig", py.None())?,
    }
    Ok(d)
}

fn get<'py, T: for<'a> FromPyObject<'a, 'py>>(d: &Bound<'py, PyAny>, key: &str) -> PyResult<T> {
    d.get_item(key)?.extract::<T>().map_err(Into::into)
}

fn node_from_py(d: &Bound<'_, PyAny>) -> PyResult<SyntaxNode> {
    let kind: String = get(d, "kind")?;
    if !syntax::is_ident(&kind) {
        return Err(PyValueError::new_err(format!("`{kind}` is not a valid node kind")));
    }
    let mut attrs = Vec::new();
    for a in d.get_item("attrs")?.try_iter()? {
        let a = a?;
        let orig = a.get_item("orig")?;
        attrs.push(SyntaxAttr {
            key: get(&a, "key")?,
            value: get(&a, "value")?,
            leading: get(&a, "leading")?,
            trailing: get(&a, "trailing")?,
            orig: if orig.is_none() { None } else { Some(orig.cast::<AttrToken>()?.get().0.clone()) },
        });
    }
    let children = d.get_item("children")?;
    let children = if children.is_none() {
        None
    } else {
        Some(children.try_iter()?.map(|c| node_from_py(&c?)).collect::<PyResult<_>>()?)
    };
    let orig = d.get_item("orig")?;
    Ok(SyntaxNode {
        kind,
        name: get(d, "name")?,
        attrs,
        children,
        leading: get(d, "leading")?,
        orig: if orig.is_none() { None } else { Some(orig.cast::<NodeToken>()?.get().0.clone()) },
    })
}

/// Parse source into `(nodes, tail, indent)`.
#[pyfunction]
fn parse<'py>(py: Python<'py>, src: &str) -> PyResult<(Bound<'py, PyList>, String, String)> {
    let doc = syntax::parse(src).map_err(value_err)?;
    let nodes = PyList::empty(py);
    for n in &doc.nodes {
        nodes.append(node_to_py(py, n)?)?;
    }
    Ok((nodes, doc.tail, doc.indent))
}

/// Print `(nodes, tail, indent)` back to `(source, spans)`; `spans` holds
/// each node's byte range in pre-order.
#[pyfunction]
#[pyo3(name = "print")]
fn print_doc(
    nodes: &Bound<'_, PyAny>,
    tail: String,
    indent: String,
) -> PyResult<(String, Vec<(usize, usize)>)> {
    let nodes = nodes.try_iter()?.map(|n| node_from_py(&n?)).collect::<PyResult<_>>()?;
    let printed = syntax::print(&SyntaxDoc { nodes, tail, indent });
    Ok((printed.text, printed.spans))
}

// --- values -----------------------------------------------------------------

/// Validate `raw` as the value of `key`; returns it trimmed.
#[pyfunction]
fn check_value(key: &str, raw: &str) -> PyResult<String> {
    syntax::check_value(key, raw).map_err(value_err)
}

/// f32 → the f64 a human wrote (`0.3`, not `0.30000001192092896`).
fn tidy(x: f32) -> f64 {
    x.to_string().parse().unwrap_or(x as f64)
}

/// Decode `raw` to `(tag, payload)`: `("num", float)`, `("str", str)`,
/// `("ident", str)`, `("list", nested lists)`, or `("raw", source)` for
/// anything symbolic (expressions, gradients, `face(...)` lists).
#[pyfunction]
fn decode_value<'py>(py: Python<'py>, key: &str, raw: &str) -> PyResult<(&'static str, Bound<'py, PyAny>)> {
    let v = syntax::parse_value(key, raw).map_err(value_err)?;
    let list = |rows: Vec<Vec<f64>>| -> PyResult<Bound<'py, PyAny>> {
        Ok(PyList::new(py, rows)?.into_any())
    };
    Ok(match v {
        Value::Number(n) => ("num", tidy(n).into_pyobject(py)?.into_any()),
        Value::String(s) => ("str", s.into_pyobject(py)?.into_any()),
        Value::Ident(s) => ("ident", s.into_pyobject(py)?.into_any()),
        Value::Vec3(v) => ("list", PyList::new(py, v.map(tidy))?.into_any()),
        Value::List(v) => ("list", PyList::new(py, v.into_iter().map(tidy))?.into_any()),
        Value::ListVec3(v) => ("list", list(v.iter().map(|r| r.map(tidy).to_vec()).collect())?),
        Value::ListPair(v) => ("list", list(v.iter().map(|r| r.map(tidy).to_vec()).collect())?),
        Value::ListQuad(v) => ("list", list(v.iter().map(|r| r.map(tidy).to_vec()).collect())?),
        Value::ListString(v) => ("list", PyList::new(py, v)?.into_any()),
        Value::Expr(_)
        | Value::Vec3Expr(_)
        | Value::ListExpr(_)
        | Value::Gradient(_)
        | Value::FaceList(_) => ("raw", raw.trim().into_pyobject(py)?.into_any()),
    })
}

#[pyfunction]
fn format_number(x: f64) -> PyResult<String> {
    syntax::format_number(x).map_err(value_err)
}

#[pyfunction]
fn quote(s: &str) -> String {
    syntax::quote(s)
}

#[pyfunction]
fn is_ident(s: &str) -> bool {
    syntax::is_ident(s)
}

// --- compile ----------------------------------------------------------------

fn diag_to_py<'py>(py: Python<'py>, d: &Diagnostic) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    let severity = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    };
    out.set_item("severity", severity)?;
    out.set_item("code", &d.code)?;
    out.set_item("message", &d.message)?;
    out.set_item("span", d.span.map(|s| (s.start, s.end)))?;
    out.set_item("file", &d.file)?;
    Ok(out)
}

fn diags_to_py<'py>(py: Python<'py>, diags: &[Diagnostic]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for d in diags {
        list.append(diag_to_py(py, d)?)?;
    }
    Ok(list)
}

/// Run every stage short of export; returns the diagnostics.
#[pyfunction]
#[pyo3(signature = (src, base_dir=None))]
fn check<'py>(py: Python<'py>, src: String, base_dir: Option<PathBuf>) -> PyResult<Bound<'py, PyList>> {
    let out = py.detach(|| crate::check(&src, base_dir.as_deref()));
    diags_to_py(py, &out.diagnostics)
}

/// Compile to `(bytes | None, diagnostics)`. `format` is `"glb"` or `"fbx"`.
#[pyfunction]
#[pyo3(signature = (src, base_dir=None, format="glb"))]
fn compile<'py>(
    py: Python<'py>,
    src: String,
    base_dir: Option<PathBuf>,
    format: &str,
) -> PyResult<(Option<Bound<'py, PyBytes>>, Bound<'py, PyList>)> {
    let format = match format.to_ascii_lowercase().as_str() {
        "glb" => Format::Glb,
        "fbx" => Format::Fbx,
        other => return Err(PyValueError::new_err(format!("unknown format `{other}` (glb, fbx)"))),
    };
    let out = py.detach(|| crate::compile(&src, base_dir.as_deref(), format));
    let bytes = out.output.map(|b| PyBytes::new(py, &b));
    Ok((bytes, diags_to_py(py, &out.diagnostics)?))
}

#[pyfunction]
fn read_source(path: PathBuf) -> PyResult<String> {
    crate::read_source(&path).map_err(|e| PyIOError::new_err(format!("{}: {e:#}", path.display())))
}

#[pyfunction]
fn write_source(path: PathBuf, src: &str) -> PyResult<()> {
    crate::write_source(&path, src)
        .map_err(|e| PyIOError::new_err(format!("{}: {e:#}", path.display())))
}

/// `[(kind, [attr, …]), …]` from the validator's schema, in declaration order.
#[pyfunction]
fn kinds() -> Vec<(&'static str, Vec<&'static str>)> {
    mogen_validate::KNOWN_KINDS
        .iter()
        .map(|k| (*k, mogen_validate::attrs_for_kind(k).to_vec()))
        .collect()
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<NodeToken>()?;
    m.add_class::<AttrToken>()?;
    m.add_function(wrap_pyfunction!(parse, m)?)?;
    m.add_function(wrap_pyfunction!(print_doc, m)?)?;
    m.add_function(wrap_pyfunction!(check_value, m)?)?;
    m.add_function(wrap_pyfunction!(decode_value, m)?)?;
    m.add_function(wrap_pyfunction!(format_number, m)?)?;
    m.add_function(wrap_pyfunction!(quote, m)?)?;
    m.add_function(wrap_pyfunction!(is_ident, m)?)?;
    m.add_function(wrap_pyfunction!(check, m)?)?;
    m.add_function(wrap_pyfunction!(compile, m)?)?;
    m.add_function(wrap_pyfunction!(read_source, m)?)?;
    m.add_function(wrap_pyfunction!(write_source, m)?)?;
    m.add_function(wrap_pyfunction!(kinds, m)?)?;
    Ok(())
}
