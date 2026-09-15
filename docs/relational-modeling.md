# Relational modeling proposal and contract

`relate` extends existing connectors, `attach` and `conform`; ordinary placement
continues to work. Version 1 supports these bounded operations:

```mog
scene {
  box "seat" (pos=[0,0.6,0],size=[1,0.1,0.7])
  plane "floor" (size=[2,0,2],tags="floating")
  spline_tube "leg" (points=[[0,0,0],[0,0.3,0]],radius=0.025)
  box "foot" (size=[0.08,0.04,0.08])
}
relate (child="leg",target="seat",mode="endpoint",endpoint="end",socket="bottom",insertion=0.015)
relate (child="foot",target="floor",mode="ground",socket="top",clearance=0)
```

- `endpoint`: move `start` or `end` of an authored sweep/spline path to a
  target connector, then retessellate that path. This changes geometry, not
  its node transform. Intermediate controls remain authored. It anchors the
  path centerline; it is not a miter cut or a general surface deformation.
- `ground`: translate the child's subtree until its lowest actual mesh
  vertex along the connector normal lies on the connector plane plus
  `clearance`. Use a plane's top connector for a floor.
- `align`: translate the child's `plug` (default `bottom`) onto the target
  `socket` (default `top`), preserving orientation. Use existing `attach`
  when you want connector rotation and reparenting as well.

`offset=[x,y,z]` is in target-local coordinates. `insertion` and `clearance`
are nonnegative world-space lengths: destination = transformed connector +
transformed offset + unit world normal × (clearance − insertion). Use one
or the other. The normal transforms by the inverse transpose. This supports
rotated and scaled parents without interpreting local distances as metres.
`tolerance` defaults to 0.0001 scene units (metres after unit lowering).

Relationships resolve after `attach` and `conform`, before skin binding,
colliders and physics. Dependencies include target/child ancestry. Resolution
is deterministic and atomic: all operations apply to a temporary graph, then
commit together. Missing/ambiguous references, cycles and conflicting writes
produce source-aware E0150 errors. A child can have two endpoint constraints
(one per end), or one rigid relationship. Module instances resolve using the
same expansion scopes as attach. Relationships inside array/mirror/grid/stack
are rejected in v1; instantiate a parameterized module explicitly instead.

Endpoint retessellation supports plain `sweep`, `spline_tube` and
`spline_ribbon` paths. Closed paths, anchored/subdivided/deformed paths,
conformed children, child geometry and prebound skins are rejected instead of
pretending the constraint remains satisfied. Targets may be ordinary named
connectors; the connector plane is a semantic location, not proof of surface
contact. Gap/contact inspection supplies that separate evidence.

Compiled geometry and transform locks remain the authority: changing a driver
that moves or reshapes a locked dependent fails the existing compiled lock
comparison. Inspection lists resolved relations and their affected nodes.
