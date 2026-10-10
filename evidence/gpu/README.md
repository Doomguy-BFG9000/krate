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
- m4-bistro-krate-window.jpg, m4-bistro-native-window.jpg -- Amazon
  Lumberyard Bistro (exterior, 2.8 million triangles, 405 BC-compressed
  textures) in Bevy 0.19.1, loaded by Bevy's own glTF loader through std::fs.
  The first is the Krate app in a Krate window, the second the same scene
  code built natively (winit, wgpu's Metal backend), same camera, 1280x720:
  the frames match (mean difference 0.000 per channel, largest 4/255).
  2026-10-11, machine shared with other builds (load average 20-45):
  Krate window 36-44 fps; native 57-60 fps multi-threaded, 60-66 fps
  single-threaded. Headless unpaced at 2560x1440: 25.9 fps with the
  blocking read-back and blocking buffer maps, 28.2 with the two-buffer
  read-back, 42.9 with maps that finish asynchronously. Compile cache: 4.9 s
  -> 0.35 s to the app's first line.
- m4-bistro-shoot.jpg -- `krate run --headless --shoot` of the same app with
  KRATE_SHOOT_AFTER_MS=25000: a drawn frame, not the first black one (K-1033).

Bistro: Amazon Lumberyard Bistro, Open Research Content Archive (ORCA),
Amazon Lumberyard, 2017, http://developer.nvidia.com/orca/amazon-lumberyard-bistro,
CC-BY 4.0. Converted to glTF with Blender 5.2.2 (scripts in krate-bevy).
