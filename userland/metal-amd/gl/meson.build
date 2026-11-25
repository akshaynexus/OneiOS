# Copyright (c) 2026 iOSPlus10 contributors, OneiOS contributors
# SPDX-License-Identifier: MIT
#
# OneiOSRadeonGL (userland/metal-amd/gl): Mesa's GLES frontend over the A56's Gallium screen as the GLD/GLI plug-in
# of iOS 16.5's OpenGLES (OneiOSRadeonGL.bundle/OneiOSRadeonGL), and its host test. This file is the meson of a
# Gallium target inside Mesa's tree: userland/metal-amd/gl/build.sh copies userland/metal-amd/gl/ to
# MESA/src/gallium/targets/oneios-gl/, and patches/mesa/5101-oneios-gl-target.patch adds the subdir() that reaches
# it (with GLES built). Linking follows the libgl-xlib target (NegroniGL's meson.build, iOSPlus10, followed osmesa's).
#
# The screen: radeonsi through oneios_radeonsi_screen_create (K3, the guest Mesa stage's
# targets/oneios-radeonsi/oneios_radeonsi.c, compiled in here, so that the GL frontend and radeonsi are one Mesa: a
# second copy in the process would not share NIR's type singletons) when the tree has it and radeonsi is built;
# softpipe always (gl_core.c says when it may stand in).

prog_sh = find_program('sh')

ogl_gli_c = custom_target(
  'gli_dispatch.c',
  input : ['gen-gli.sh', 'gli-slots.txt', main_dispatch_h],
  output : 'gli_dispatch.c',
  command : [prog_sh, '@INPUT0@', '@INPUT1@', join_paths(meson.project_source_root(), 'include'), '@INPUT2@',
             '@OUTPUT@'],
)

ogl_gld = custom_target(
  'gld_stubs',
  input : ['plugin/gen-gld.sh', 'plugin/gld-names.txt', 'plugin/gld.c'],
  output : ['gld_stubs.c', 'gld-exports.txt'],
  command : [prog_sh, '@INPUT0@', '@INPUT1@', '@INPUT2@', '@OUTPUT0@', '@OUTPUT1@'],
)

ogl_core_src = files('gl_core.c', 'gl_hand.c', 'view.c', 'surface.c', 'shaders.c', 'sw_winsys.c', 'log.c')
ogl_plugin_src = files('plugin/gli_table.c', 'plugin/gli_context.c', 'plugin/gli_view.c', 'plugin/gld.c')
ogl_inc = [
  include_directories('.', 'plugin'), inc_include, inc_src, inc_mesa, inc_gallium, inc_gallium_aux,
  inc_gallium_winsys, inc_gallium_drivers, include_directories('../../../mesa/glapi/glapi/gen'),
]
ogl_deps = [dep_thread, dep_clock, idep_mesautil, driver_softpipe]
ogl_c_args = [
  '-Wno-multichar',
  '-Wno-unused-parameter', '-Wno-missing-field-initializers', '-Wno-deprecated-declarations',
  '-Wno-missing-prototypes', '-DONEIOS_GL_MESA_VERSION="@0@"'.format(meson.project_version()),
]
ogl_gen_h = [main_dispatch_h, main_marshal_generated_h]

# K3: radeonsi, when this tree is the guest Mesa stage's (patches 0001-0006: targets/oneios-radeonsi) and builds it
ogl_k3 = files()
if with_gallium_radeonsi and fs.exists('../oneios-radeonsi/oneios_radeonsi.c')
  ogl_k3 = files('../oneios-radeonsi/oneios_radeonsi.c')
  ogl_c_args += '-DONEIOS_GL_RADEONSI=1'
  ogl_inc += [include_directories('../oneios-radeonsi'), get_variable('inc_amd_common', []),
              get_variable('inc_amd', []), get_variable('inc_virtio', []), get_variable('inc_virtio_gpu', [])]
  if get_variable('_oneios_darwin_drm', false)
    ogl_inc += include_directories('../../../oneios')
  endif
  ogl_deps += [get_variable('dep_libdrm', []), get_variable('dep_libdrm_amdgpu', []), driver_radeonsi,
               idep_xmlconfig]
endif

ogl_link_with = [libmesa, libgallium, libglapi, libgalliumvl_stub, libws_null]

if host_machine.system() == 'darwin'
  # OneiOSRadeonGL.bundle/OneiOSRadeonGL (build.sh makes the bundle around this file). Its exports are the 129 GLD
  # names libGFXShared looks up, gldInitialize/TerminateLibrary, the 18 GLI names eagl_init binds, gliTerminateLibrary
  # and oneios_gl_probe (plugin/gen-gld.sh). MH_BUNDLE with chained fixups: a bundle otherwise links with lazy stubs
  # that import dyld_stub_binder, which iOS 16's shared cache does not export (NegroniGL's finding on 20H392).
  oneios_gl_bundle = shared_module(
    'OneiOSRadeonGL',
    ogl_core_src, ogl_plugin_src, ogl_gli_c, ogl_gld[0], ogl_k3, ogl_gen_h,
    name_prefix : '',
    name_suffix : 'bin',
    gnu_symbol_visibility : 'hidden',
    c_args : ogl_c_args,
    link_args : [
      '-Wl,-exported_symbols_list,' + ogl_gld[1].full_path(), '-Wl,-dead_strip', '-Wl,-fixup_chains',
      '-framework', 'IOKit', '-framework', 'IOSurface', '-framework', 'CoreFoundation',
    ],
    link_depends : ogl_gld[1],
    include_directories : ogl_inc,
    link_whole : [libglapi_bridge],
    link_with : ogl_link_with,
    dependencies : ogl_deps,
    build_by_default : false,
    install : false,
  )
  # oneios-gl-probe: the lasting guest tool (tests/oneios_gl_probe.c): loads the bundle, runs oneios_gl_probe
  executable(
    'oneios-gl-probe',
    files('tests/oneios_gl_probe.c'),
    build_by_default : false,
    install : false,
  )
else
  # the host test: the same sources, the plug-in's GLI entries called as EAGL calls them, IOSurfaces of host/
  executable(
    'oneios_gl_test',
    ogl_core_src, ogl_plugin_src, ogl_gli_c, ogl_gld[0], ogl_k3, ogl_gen_h,
    files('host/iosurface_host.c', 'tests/oneios_gl_test.c'),
    c_args : ogl_c_args,
    include_directories : ogl_inc + [include_directories('host')],
    link_whole : [libglapi_bridge],
    link_with : ogl_link_with,
    dependencies : ogl_deps + [dep_dl],
    build_by_default : false,
    install : false,
  )
endif
