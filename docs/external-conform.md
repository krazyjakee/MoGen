# Conform across module instances

Plain `target="body_shell"` and `child="seam"` names retain module visibility
and strict replicated-subtree scope. An external target must be explicit:

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

`/car/body_shell` identifies a root node named `car`, then its direct child
`body_shell`. Each segment must be unique among those siblings. `scene` is a
transparent declaration, **not a path segment**. There is no fuzzy/global name
fallback and no `..` escape. Absolute references are supported on the target;
the child remains local, so a module cannot modify an unrelated external child.

See [the runnable two-door example](../examples/features/external_conform.mog).
A module instantiated twice keeps two separate local `seam` identities. Numeric
parameters still interpolate connector names. Both detail instances bind to the
same explicit body without duplicating its geometry or expanding detail
operations at scene scope. A containing group's transforms and reflections are
handled by the existing target/child coordinate conversion.

Resolution runs after lowering and attachments, before skin binding. Paths are
resolved before conform reparenting. When one binding deforms another binding's
target, the target runs first. Cycles fail atomically. Inside array/mirror
expansion, targets must already be lowered; forward external references from
that early pass are unsupported and receive placement guidance. The connector
belongs to the target and is expressed in target-local coordinates; the
existing world-to-child conversion preserves transformed parents. `reparent=0`
keeps the child's parent; the default reparents after deformation.

The original failure, `conform: unknown target node "body_shell"` inside
`door_side`, now distinguishes a missing name from a present but inaccessible
name, with declaration byte span, use identity and scope. Replace the local
spelling with the exact absolute target path, or declare the binding at scene
scope after instantiation. Duplicate visible names fail as ambiguous.

Inspect reports `conform_binding` with the resolved target node and coordinate
parameters. Editing a target recompiles its dependents; existing compiled
geometry/subtree locks reject indirect changes to locked details.
