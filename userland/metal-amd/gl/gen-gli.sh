#!/bin/sh
# Copyright (c) 2026 iOSPlus10 contributors, OneiOS contributors
# SPDX-License-Identifier: MIT
#
# OneiOSRadeonGL: the GLI dispatch table CoreAnimation calls, as C over Mesa's GL dispatch. The POSIX sh + awk port
# of iOSPlus10 NegroniGL's gen-gli.py (Python is not used in this tree: CLAUDE.md); the rules are the same.
#
#   gen-gli.sh gli-slots.txt MESA_INCLUDE_DIR dispatch.h OUT.c
#
# Slot i is called by CoreAnimation as disp[i](rctx, arguments of the GL function of slot i). Each wrapper makes
# rctx's context current when it is not, then calls Mesa through the current dispatch table:
# CALL_<Name>(GET_DISPATCH(), (...)) of main/dispatch.h (Mesa's build makes it: src/mesa/glapi/glapi/gen). The target
# is the core name (suffix ARB/EXT/OES/APPLE/NV/KHR dropped) if Mesa's table has it with as many parameters, else the
# name itself, else the name with ARB, EXT or OES. Prototypes (parameter names and types) come from the Khronos headers
# in Mesa's include/. Names in HAND are written in gl_hand.c as ngl_h_<Name>(rctx, ...). Slots with no Mesa function
# log once (ngl_unsupported) and return 0. A parameter named "target" goes through NGL_TGT (rectangle textures are 2D
# ones here). Prints one line to standard error: how many slots are named and which have no Mesa function.
set -eu
[ $# -eq 4 ] || { echo "usage: gen-gli.sh gli-slots.txt MESA_INCLUDE_DIR dispatch.h OUT.c" >&2; exit 2; }
slots=$1 inc=$2 dispatch=$3 out=$4
headers=
for h in GL/gl.h GL/glext.h GLES2/gl2.h GLES2/gl2ext.h GLES3/gl3.h GLES3/gl31.h GLES3/gl32.h; do
  [ -f "$inc/$h" ] && headers="$headers $inc/$h"
done
[ -f "$dispatch" ] || { echo "gen-gli: no $dispatch" >&2; exit 1; }
tmp=$out.tmp
# shellcheck disable=SC2086
awk -v slots_file="$slots" -v dispatch_file="$dispatch" -v out="$tmp" '
function trim(s) { sub(/^[ \t\r\n]+/, "", s); sub(/[ \t\r\n]+$/, "", s); return s }
function squash(s) { gsub(/[ \t\r\n]+/, " ", s); return trim(s) }
function bare(s) { gsub(/[ \t]/, "", s); return s }
function is_ptr(t) { return index(t, "*") > 0 || bare(t) == "GLhandleARB" || bare(t) == "GLsync" || bare(t) == "GLeglImageOES" }
function conv(val, src, dst) {
  if (bare(src) == bare(dst)) return val
  if (is_ptr(src) != is_ptr(dst)) return "(" dst ")(uintptr_t)(" val ")"
  return "(" dst ")(" val ")"
}
# one prototype statement (GLAPI ... ;) into protos[name] = ret, pn[name], pt[name,i], pv[name,i]
function proto(s,    m, ret, name, rest, args, n, a, i, p, at) {
  gsub(/\/\*[^*]*\*+([^\/*][^*]*\*+)*\//, " ", s)
  s = squash(s)
  if (!match(s, /(GLAPIENTRY|APIENTRY|GL_APIENTRY) +gl[A-Za-z0-9_]+ *\(/)) return
  ret = substr(s, 1, RSTART - 1); sub(/^(GLAPI|GL_APICALL) +/, "", ret); ret = trim(ret)
  rest = substr(s, RSTART, RLENGTH); sub(/^(GLAPIENTRY|APIENTRY|GL_APIENTRY) +/, "", rest); sub(/ *\($/, "", rest)
  name = rest
  if (name in protos) return
  args = substr(s, RSTART + RLENGTH); sub(/\) *;.*$/, "", args); args = trim(args)
  if (args == "" || args == "void") n = 0
  else n = split(args, a, ",")
  for (i = 1; i <= n; i++) {
    p = trim(a[i])
    if (!match(p, /[A-Za-z_][A-Za-z0-9_]*$/)) return      # an unnamed or array parameter: not this generator
    pv[name, i] = substr(p, RSTART); pt[name, i] = trim(substr(p, 1, RSTART - 1))
    if (pt[name, i] == "") return
  }
  protos[name] = ret; pn[name] = n
}
FILENAME == slots_file {
  sub(/#.*/, "")
  if (NF >= 2) { slot_name[$1 + 0] = $NF; named++ }
  next
}
FILENAME == dispatch_file {
  if ($0 ~ /^typedef .*\(GLAPIENTRYP _glptr_[A-Za-z0-9_]+\)\(/) {
    s = $0; sub(/^typedef +/, "", s)
    r = s; sub(/ *\(GLAPIENTRYP _glptr_.*$/, "", r)
    nm = s; sub(/^.*\(GLAPIENTRYP _glptr_/, "", nm); sub(/\).*$/, "", nm)
    ar = s; sub(/^[^)]*\)\(/, "", ar); sub(/\);.*$/, "", ar); ar = trim(ar)
    if (ar == "" || ar == "void") k = 0; else k = split(ar, b, ",")
    disp[nm] = trim(r); dn[nm] = k
    for (i = 1; i <= k; i++) dt[nm, i] = trim(b[i])
  }
  next
}
{
  if (buf != "") { buf = buf " " $0; if (index($0, ";")) { proto(buf); buf = "" }; next }
  if ($0 ~ /^(GLAPI|GL_APICALL)[ \t]/) { if (index($0, ";")) proto($0); else buf = $0 }
}
END {
  NS = 1036
  HAND["glDeleteProgram"] = "ngl_h_DeleteObject"      # Apple slot 591 is DeleteObjectARB: shader or program
  HAND["glShaderSource"] = "ngl_h_ShaderSource"; HAND["glCompileShader"] = "ngl_h_CompileShader"
  HAND["glLinkProgram"] = "ngl_h_LinkProgram"; HAND["glUseProgram"] = "ngl_h_UseProgram"
  HAND["glUniformMatrix4fv"] = "ngl_h_UniformMatrix4fv"; HAND["glGetString"] = "ngl_h_GetString"
  HAND["glTexImage2D"] = "ngl_h_TexImage2D"; HAND["glBindTexture"] = "ngl_h_BindTexture"
  HAND["glDeleteTextures"] = "ngl_h_DeleteTextures"; HAND["glDeleteRenderbuffers"] = "ngl_h_DeleteRenderbuffers"
  HAND["glFramebufferTexture2D"] = "ngl_h_FramebufferTexture2D"; HAND["glClear"] = "ngl_h_Clear"
  HAND["glDrawArrays"] = "ngl_h_DrawArrays"; HAND["glDrawElements"] = "ngl_h_DrawElements"
  HAND["glFlush"] = "ngl_h_Flush"; HAND["glFinish"] = "ngl_h_Finish"
  HAND["glFinishObjectAPPLE"] = "ngl_h_FinishObjectAPPLE"; HAND["glTestObjectAPPLE"] = "ngl_h_TestObjectAPPLE"
  HAND["glFinishFenceAPPLE"] = "ngl_h_FinishFenceAPPLE"
  HAND["glGenFencesAPPLE"] = "ngl_h_GenFencesAPPLE"; HAND["glDeleteFencesAPPLE"] = "ngl_h_DeleteFencesAPPLE"
  HAND["glSetFenceAPPLE"] = "ngl_h_SetFenceAPPLE"; HAND["glIsFenceAPPLE"] = "ngl_h_IsFenceAPPLE"
  HAND["glTestFenceAPPLE"] = "ngl_h_TestFenceAPPLE"
  ALIAS["CreateShaderObject"] = "CreateShader"; ALIAS["CreateProgramObject"] = "CreateProgram"
  ALIAS["UseProgramObject"] = "UseProgram"; ALIAS["AttachObject"] = "AttachShader"; ALIAS["DetachObject"] = "DetachShader"
  nmiss = 0; missing = ""; hands = 0
  print "/* generated by userland/metal-amd/gl/gen-gli.sh from gli-slots.txt: do not edit */" > out
  print "#include <stdint.h>\n#include \"oneios_gl.h\"\n#include \"mesa/glapi/glapi/glapi.h\"\n#include \"dispatch.h\"\n" > out
  for (i = 0; i < NS; i++) {
    name = (i in slot_name) ? slot_name[i] : ""
    if (name in HAND) { table[i] = "(void *)" HAND[name]; hands++; continue }
    target = ""
    if (name != "" && (name in protos)) {
      base = substr(name, 3); core = base; sub(/(ARB|EXT|OES|APPLE|NV|KHR)$/, "", core)
      if (core in ALIAS) core = ALIAS[core]
      c[1] = core; c[2] = base; c[3] = core "ARB"; c[4] = core "EXT"; c[5] = core "OES"
      for (j = 1; j <= 5; j++) if ((c[j] in disp) && dn[c[j]] == pn[name]) { target = c[j]; break }
    }
    if (target == "") {
      table[i] = "(void *)unsupported_" i
      printf "static long unsupported_%d(void) { ngl_unsupported(%d); return 0; }\n", i, i > out
      if (name != "") { nmiss++; missing = missing " " name }
      continue
    }
    ret = protos[name]; decl = "void *rctx"; args = ""
    for (j = 1; j <= pn[name]; j++) {
      v = pv[name, j]; val = v == "target" ? "NGL_TGT(" v ")" : v
      decl = decl ", " pt[name, j] " " v
      args = args (j > 1 ? ", " : "") conv(val, pt[name, j], dt[target, j])
    }
    call = "CALL_" target "(GET_DISPATCH(), (" args "))"
    if (bare(ret) == "void") stmt = call "; NGL_POST(" i ");"
    else stmt = ret " r = " conv(call, disp[target], ret) "; NGL_POST(" i "); return r;"
    printf "static %s w_%d(%s) { NGL_CUR(rctx); %s }\n", ret, i, decl, stmt > out
    table[i] = "(void *)w_" i
  }
  printf "\nvoid *const ngl_dispatch[%d] = {\n", NS > out
  for (i = 0; i < NS; i++) printf "\t%s,\n", table[i] > out
  printf "};\n\nconst char *const ngl_slot_names[%d] = {\n", NS > out
  for (i = 0; i < NS; i++) printf "\t\"%s\",\n", (i in slot_name) ? slot_name[i] : "?" > out
  printf "};\n\nconst unsigned ngl_dispatch_missing = %d;\n", nmiss > out
  printf "gen-gli: %d slots named, %d hand-written, %d without a Mesa function:%s\n", named, hands, nmiss, missing > "/dev/stderr"
}' "$slots" "$dispatch" $headers
mv "$tmp" "$out"
