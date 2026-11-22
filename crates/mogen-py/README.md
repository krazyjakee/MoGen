# mogen (Python)

Python API for [MoGen](https://github.com/krazyjakee/MoGen): build, load, edit
and compile `.mog` files to glTF 2.0 `.glb` with the same native pipeline as
the `mogen` CLI.

```python
import mogen as mg

doc = mg.Document()
oak = doc.material("oak", color=[0.55, 0.38, 0.2], roughness=0.7)
with doc.scene():
    mg.cylinder("seat", radius=0.2, height=mg.units.cm(3), y=0.45, mat=oak)
doc.save("stool.mog")
doc.build("stool.glb")
```

Full reference: [`docs/python.md`](https://github.com/krazyjakee/MoGen/blob/master/docs/python.md).
