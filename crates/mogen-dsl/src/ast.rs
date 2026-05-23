use std::path::PathBuf;

use glam::{Quat, Vec3};
use mogen_core::Span;

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: String,
    pub name: Option<String>,
    pub attrs: Vec<(String, Value)>,
    pub children: Vec<Node>,
    pub span: Span,
    pub kind_span: Span,
    /// Module-use expansion frame this node was cloned into, or `None` for
    /// nodes the user wrote directly. Set by `expand_modules` so attach
    /// resolution can scope name lookups to a single module instance even
    /// after expansion has flattened everything into siblings.
    pub use_id: Option<u32>,
    /// Canonical path of the imported `.mog` file this node was hoisted
    /// from. `None` for nodes the user wrote in the file currently being
    /// lowered. Set by `resolve_imports` and propagated through `expand_modules`
    /// so lowering can stamp each `SceneNode`/`Material`/`Clip`/`Skin` with
    /// its origin file. Drives MoGen Studio's per-import sidebar scoping.
    pub origin: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum Value {
    Number(f32),
    Vec3([f32; 3]),
    String(String),
    Ident(String),
    /// Deferred scalar expression — carries at least one `$param` reference.
    Expr(Expr),
    /// Vec3 with at least one component that is still a deferred expression.
    Vec3Expr([Expr; 3]),
    /// N-element numeric list (arity != 3); e.g. `limits=[-90, 90]`.
    List(Vec<f32>),
    /// N-element list with at least one component that is still a deferred expression.
    ListExpr(Vec<Expr>),
    /// List of 3-component constant vectors, e.g. `points=[[0,0,0], [1,0,0]]`.
    /// Used by `spline_tube` control points.
    ListVec3(Vec<[f32; 3]>),
    /// List of 2-element constant sublists, e.g. `profile=[[0.2, 0.0], [0.3, 0.5]]`.
    /// Used by `lathe` profile rows.
    ListPair(Vec<[f32; 2]>),
    /// List of 4-element constant sublists, e.g. `holes=[[0, -0.4, 0.9, 2.0], …]`.
    /// Used by `wall` cutouts: `[x, y, w, h]` in the wall's local frame.
    ListQuad(Vec<[f32; 4]>),
    /// All-string list, e.g. `tags=["furniture", "chair"]`. Used by the
    /// top-level `meta(...)` block; mixing strings and numbers in one list
    /// is rejected at parse time.
    ListString(Vec<String>),
    /// `gradient=linear(from=[…], to=[…], axis=y)` and friends. The parser
    /// recognises the surface shape; lowering interprets per kind so the
    /// validator can attach a span to whichever attribute is wrong.
    Gradient(GradientDef),
}

/// Surface representation of a gradient value. `kind` is one of
/// `"linear" | "vertical" | "radial" | "stops"`; the inner `attrs` mirror an
/// `attr_list` payload — anything from `from=[1,0,0]` to
/// `colors=[[1,0,0], [0,1,0]]` to `axis=y` flows through unchanged. The span
/// covers the whole `kind(...)` form so a bad inner attribute can still be
/// reported against the gradient as a whole when no inner span is recorded.
#[derive(Debug, Clone)]
pub struct GradientDef {
    pub kind: String,
    pub attrs: Vec<(String, Value)>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f32),
    Param(String),
    Bin(Box<Expr>, BinOp, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    /// Comparisons evaluate to 1.0 (true) or 0.0 (false). They have lower
    /// precedence than arithmetic so `$a + 1 > 0` parses as `($a + 1) > 0`.
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

impl Expr {
    /// Evaluate with no parameter scope; succeeds only if fully constant.
    pub fn eval_const(&self) -> Option<f32> {
        match self {
            Expr::Num(n) => Some(*n),
            Expr::Param(_) => None,
            Expr::Bin(a, op, b) => {
                let a = a.eval_const()?;
                let b = b.eval_const()?;
                Some(apply(op, a, b))
            }
        }
    }

    /// Evaluate in a scope mapping `$name` → `f32`.
    pub fn eval(&self, scope: &dyn Fn(&str) -> Option<f32>) -> Option<f32> {
        match self {
            Expr::Num(n) => Some(*n),
            Expr::Param(name) => scope(name),
            Expr::Bin(a, op, b) => Some(apply(op, a.eval(scope)?, b.eval(scope)?)),
        }
    }

    /// Collect every distinct `$param` referenced by this expression.
    pub fn collect_params(&self, out: &mut Vec<String>) {
        match self {
            Expr::Num(_) => {}
            Expr::Param(name) => {
                if !out.iter().any(|n| n == name) {
                    out.push(name.clone());
                }
            }
            Expr::Bin(a, _, b) => {
                a.collect_params(out);
                b.collect_params(out);
            }
        }
    }
}

fn apply(op: &BinOp, a: f32, b: f32) -> f32 {
    match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        BinOp::Lt => if a <  b { 1.0 } else { 0.0 },
        BinOp::Le => if a <= b { 1.0 } else { 0.0 },
        BinOp::Gt => if a >  b { 1.0 } else { 0.0 },
        BinOp::Ge => if a >= b { 1.0 } else { 0.0 },
        BinOp::Eq => if a == b { 1.0 } else { 0.0 },
        BinOp::Ne => if a != b { 1.0 } else { 0.0 },
    }
}

impl Node {
    pub fn attr(&self, key: &str) -> Option<&Value> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn attr_vec3(&self, key: &str) -> Option<Vec3> {
        match self.attr(key)? {
            Value::Vec3(v) => Some(Vec3::from_array(*v)),
            _ => None,
        }
    }

    pub fn attr_pair(&self, key: &str) -> Option<[f32; 2]> {
        match self.attr(key)? {
            Value::List(v) if v.len() == 2 => Some([v[0], v[1]]),
            _ => None,
        }
    }

    pub fn attr_list(&self, key: &str) -> Option<&[f32]> {
        match self.attr(key)? {
            Value::List(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// Returns a list of 3D points, accepting either a `ListVec3` (nested
    /// vec3 literals) or a flat `List` whose length is a multiple of 3.
    pub fn attr_list_vec3(&self, key: &str) -> Option<Vec<[f32; 3]>> {
        match self.attr(key)? {
            Value::ListVec3(v) => Some(v.clone()),
            Value::List(v) if v.len() >= 3 && v.len() % 3 == 0 => Some(
                v.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
            ),
            _ => None,
        }
    }

    /// Returns a list of 2D pairs, accepting either a `ListPair` (nested
    /// 2-element sublists) or a flat `List` whose length is a multiple of 2.
    pub fn attr_list_pair(&self, key: &str) -> Option<Vec<[f32; 2]>> {
        match self.attr(key)? {
            Value::ListPair(v) => Some(v.clone()),
            Value::List(v) if v.len() >= 2 && v.len() % 2 == 0 => Some(
                v.chunks_exact(2).map(|c| [c[0], c[1]]).collect(),
            ),
            _ => None,
        }
    }

    /// Returns a list of 4-tuples, accepting either a `ListQuad` (nested
    /// 4-element sublists) or a flat `List` whose length is a multiple of 4.
    pub fn attr_list_quad(&self, key: &str) -> Option<Vec<[f32; 4]>> {
        match self.attr(key)? {
            Value::ListQuad(v) => Some(v.clone()),
            Value::List(v) if v.len() >= 4 && v.len() % 4 == 0 => Some(
                v.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect(),
            ),
            _ => None,
        }
    }

    pub fn attr_number(&self, key: &str) -> Option<f32> {
        match self.attr(key)? {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Returns `Some` for either a quoted `"…"` or a bare ident; the two are
    /// interchangeable for scalar string attributes (path-like values, role
    /// names, etc.).
    pub fn attr_string(&self, key: &str) -> Option<&str> {
        match self.attr(key)? {
            Value::String(s) | Value::Ident(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// `scale` accepts either a scalar (uniform) or a vec3.
    pub fn attr_scale(&self, key: &str) -> Option<Vec3> {
        match self.attr(key)? {
            Value::Number(n) => Some(Vec3::splat(*n)),
            Value::Vec3(v) => Some(Vec3::from_array(*v)),
            _ => None,
        }
    }

    pub fn attr_gradient(&self, key: &str) -> Option<&GradientDef> {
        match self.attr(key)? {
            Value::Gradient(g) => Some(g),
            _ => None,
        }
    }

    /// `rot=[x,y,z]` in degrees, XYZ Euler → Quat.
    pub fn attr_rotation(&self, key: &str) -> Option<Quat> {
        let v = self.attr_vec3(key)?;
        Some(Quat::from_euler(
            glam::EulerRot::XYZ,
            v.x.to_radians(),
            v.y.to_radians(),
            v.z.to_radians(),
        ))
    }
}
