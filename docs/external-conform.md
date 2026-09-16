# Conform across module instances

Plain `target="body_shell"` / `child="seam"` names are resolved in module scope.
To reach a target outside the module, use an absolute path:

```mog
module "detail" {
  box "seam" (size=[1,0.01,0.02])
  conform (target="/car/body_shell", child="seam", from="a", to="b", along=x, reparent=0)
}
scene {
  group "car" {
    box "body_shell" (size=[2,0.5,3]) {
      connector "a" (at=[-0.5,0.25,0],dir=[0,1,0])
      connector "b" (at=[0.5,0.25,0],dir=[0,1,0])
    }
    use "detail"
  }
}
```

- `/car/body_shell` = root node `car`, then its direct child `body_shell`. Each
  segment must be unique among its siblings. `scene` is not a path segment.
- No fuzzy/global fallback, no `..`. Only `target` may be absolute; `child`
  stays local.
- Each module instance keeps its own local child; numeric params still
  interpolate connector names. Parent transforms/reflections are handled.
- Connectors belong to the target, in target-local coordinates.
- `reparent=0` keeps the child's parent; default reparents after deformation.
- Resolves after lowering and attachments, before skin binding. If one binding
  deforms another's target, the target runs first. Cycles fail.
- Inside array/mirror, targets must already be lowered; forward external
  references are unsupported.
- `unknown target node` errors distinguish missing from inaccessible names. Fix:
  use the exact absolute path, or declare the conform at scene scope after the
  `use`. Duplicate visible names fail as ambiguous.
- Inspect reports `conform_binding` with the resolved target.

Example: `examples/features/external_conform.mog`.
