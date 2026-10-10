# krate:gpu evidence (IC-540, IC-916)

All on an M4 Mac, 2026-10-10, Krate 0.5.5 built from this branch. The apps are
in ~/Projects/krate-bevy (built with its build-app.sh).

- m1-cube-shot.png -- apps/krate-gpu-probe: a lit cube through krate:gpu
  directly.
- m2-wgpu-cube-window.png -- wgpu v29.0.4's own cube example, its source file
  unchanged, drawing through wgpu-krate in a Krate window. 2048x1536 canvas,
  57-59 fps (display-paced). Headless with `KRATE_GPU_UNPACED=1`, 1024x768:
  662-664 fps, 1.5 ms a frame.
- m3-bevy-pbr-window.png -- Bevy 0.19.1, renderer unchanged: PBR materials
  (metallic and roughness varied), a shadow-casting directional light and a
  shadow-casting point light, five meshes. 1280x720 canvas (1x display):
  59.2-60.0 fps in the window after the first second; 2560x1440 canvas on the
  Retina panel: 57.5-60.0 fps; headless unpaced 85-110 fps (9-12 ms a
  frame, one core busy). Startup about 9 s, peak RSS 2.1 GB.
