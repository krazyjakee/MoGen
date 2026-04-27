import * as THREE from "three";
import { GLTFLoader } from "three/addons/loaders/GLTFLoader.js";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";

export type ViewerHandle = {
  loadGlb: (bytes: Uint8Array) => Promise<void>;
  dispose: () => void;
};

export function createViewer(parent: HTMLElement): ViewerHandle {
  const scene = new THREE.Scene();
  scene.background = new THREE.Color(0x1a1b1e);

  const renderer = new THREE.WebGLRenderer({ antialias: true });
  renderer.setPixelRatio(window.devicePixelRatio);
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  parent.appendChild(renderer.domElement);

  const camera = new THREE.PerspectiveCamera(45, 1, 0.01, 1000);
  camera.position.set(1.2, 1.0, 1.6);

  const controls = new OrbitControls(camera, renderer.domElement);
  controls.enableDamping = true;
  controls.dampingFactor = 0.08;

  // Three-point-ish lighting. Hemisphere fills shadow areas with cool sky /
  // warm ground, then a key directional light gives form. With glTF's PBR
  // pipeline this is enough to read shape without an environment map.
  scene.add(new THREE.HemisphereLight(0xbcd5ff, 0x2b1d10, 0.7));
  const key = new THREE.DirectionalLight(0xffffff, 1.4);
  key.position.set(2, 4, 2);
  scene.add(key);
  const fill = new THREE.DirectionalLight(0xffffff, 0.4);
  fill.position.set(-2, 1, -1);
  scene.add(fill);

  const grid = new THREE.GridHelper(4, 16, 0x333740, 0x2a2d33);
  (grid.material as THREE.Material).transparent = true;
  (grid.material as THREE.Material).opacity = 0.6;
  scene.add(grid);
  const axes = new THREE.AxesHelper(0.25);
  scene.add(axes);

  // Holds the currently-loaded model so we can dispose its GPU resources
  // before swapping a new one in. Otherwise rapid edits leak buffers.
  let current: THREE.Object3D | null = null;
  let currentMixer: THREE.AnimationMixer | null = null;

  const loader = new GLTFLoader();
  const clock = new THREE.Clock();

  function disposeCurrent() {
    if (!current) return;
    scene.remove(current);
    current.traverse((o) => {
      const mesh = o as THREE.Mesh;
      if (mesh.isMesh) {
        mesh.geometry?.dispose();
        const m = mesh.material;
        if (Array.isArray(m)) m.forEach((mm) => mm.dispose());
        else m?.dispose();
      }
    });
    current = null;
    currentMixer?.stopAllAction();
    currentMixer = null;
  }

  function fitCamera(object: THREE.Object3D) {
    const box = new THREE.Box3().setFromObject(object);
    if (!isFinite(box.min.x)) return;
    const size = box.getSize(new THREE.Vector3());
    const center = box.getCenter(new THREE.Vector3());
    controls.target.copy(center);

    const maxDim = Math.max(size.x, size.y, size.z) || 1;
    const fov = (camera.fov * Math.PI) / 180;
    const dist = maxDim / (2 * Math.tan(fov / 2)) * 1.6;
    const dir = new THREE.Vector3(1, 0.7, 1.2).normalize();
    camera.position.copy(center).addScaledVector(dir, dist);
    camera.near = Math.max(dist / 100, 0.001);
    camera.far = dist * 100;
    camera.updateProjectionMatrix();
  }

  function loadGlb(bytes: Uint8Array): Promise<void> {
    return new Promise((resolve, reject) => {
      // GLTFLoader.parse takes an ArrayBuffer; copy into a fresh, plain one to
      // avoid any concerns about the Uint8Array view's underlying buffer
      // being a SharedArrayBuffer or larger than the view.
      const ab = new ArrayBuffer(bytes.byteLength);
      new Uint8Array(ab).set(bytes);
      loader.parse(
        ab,
        "",
        (gltf) => {
          disposeCurrent();
          current = gltf.scene;
          scene.add(current);
          if (gltf.animations && gltf.animations.length > 0) {
            currentMixer = new THREE.AnimationMixer(current);
            for (const clip of gltf.animations) {
              currentMixer.clipAction(clip).play();
            }
          }
          fitCamera(current);
          resolve();
        },
        (err) => reject(err),
      );
    });
  }

  function resize() {
    const w = parent.clientWidth;
    const h = parent.clientHeight;
    renderer.setSize(w, h, false);
    camera.aspect = w / Math.max(h, 1);
    camera.updateProjectionMatrix();
  }
  resize();
  const ro = new ResizeObserver(resize);
  ro.observe(parent);

  let raf = 0;
  function tick() {
    raf = requestAnimationFrame(tick);
    const dt = clock.getDelta();
    if (currentMixer) currentMixer.update(dt);
    controls.update();
    renderer.render(scene, camera);
  }
  tick();

  return {
    loadGlb,
    dispose() {
      cancelAnimationFrame(raf);
      ro.disconnect();
      disposeCurrent();
      renderer.dispose();
      renderer.domElement.remove();
    },
  };
}
