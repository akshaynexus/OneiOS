// SPDX-License-Identifier: MIT
#include "xnuloader/adt.h"
#include <stdlib.h>
#include <string.h>

static void *xmalloc(size_t n) { void *p = calloc(1, n ? n : 1); if (!p) abort(); return p; }

xl_adt_node *xl_adt_new(const char *name)
{
  xl_adt_node *n = xmalloc(sizeof *n);
  xl_adt_set_str(n, "name", name);
  return n;
}

void xl_adt_free(xl_adt_node *n)
{
  if (!n) return;
  for (unsigned i = 0; i < n->nprops; i++) free(n->props[i].value);
  for (unsigned i = 0; i < n->nchildren; i++) xl_adt_free(n->children[i]);
  free(n->props);
  free(n->children);
  free(n);
}

const xl_adt_prop *xl_adt_get(const xl_adt_node *n, const char *name)
{
  for (unsigned i = 0; i < n->nprops; i++)
    if (!strncmp(n->props[i].name, name, XL_ADT_NAME_MAX)) return &n->props[i];
  return NULL;
}

const char *xl_adt_name(const xl_adt_node *n)
{
  const xl_adt_prop *p = xl_adt_get(n, "name");
  return p && p->length && p->value[p->length - 1] == 0 ? (const char *)p->value : "";
}

int xl_adt_set(xl_adt_node *n, const char *name, const void *value, size_t length)
{
  if (strlen(name) >= XL_ADT_NAME_MAX || length > 0x7fffffff) return -1;
  xl_adt_prop *p = (xl_adt_prop *)xl_adt_get(n, name);
  if (!p) {
    n->props = realloc(n->props, (n->nprops + 1) * sizeof *n->props);
    if (!n->props) abort();
    p = &n->props[n->nprops++];
    memset(p, 0, sizeof *p);
    strcpy(p->name, name);
  }
  uint8_t *copy = xmalloc(length);
  if (length) memcpy(copy, value, length);
  free(p->value);
  p->value = copy;
  p->length = (uint32_t)length;
  return 0;
}

int xl_adt_set_u32(xl_adt_node *n, const char *name, uint32_t v) { return xl_adt_set(n, name, &v, 4); }
int xl_adt_set_u64(xl_adt_node *n, const char *name, uint64_t v) { return xl_adt_set(n, name, &v, 8); }
int xl_adt_set_str(xl_adt_node *n, const char *name, const char *v) { return xl_adt_set(n, name, v, strlen(v) + 1); }

int xl_adt_unset(xl_adt_node *n, const char *name)
{
  for (unsigned i = 0; i < n->nprops; i++) {
    if (strncmp(n->props[i].name, name, XL_ADT_NAME_MAX)) continue;
    free(n->props[i].value);
    memmove(&n->props[i], &n->props[i + 1], (n->nprops - i - 1) * sizeof *n->props);
    n->nprops--;
    return 0;
  }
  return -1;
}

xl_adt_node *xl_adt_child(const xl_adt_node *n, const char *name)
{
  for (unsigned i = 0; i < n->nchildren; i++)
    if (!strcmp(xl_adt_name(n->children[i]), name)) return n->children[i];
  return NULL;
}

static xl_adt_node *attach(xl_adt_node *n, xl_adt_node *child)
{
  n->children = realloc(n->children, (n->nchildren + 1) * sizeof *n->children);
  if (!n->children) abort();
  n->children[n->nchildren++] = child;
  return child;
}

xl_adt_node *xl_adt_add(xl_adt_node *n, const char *name)
{
  xl_adt_node *c = xl_adt_child(n, name);
  return c ? c : attach(n, xl_adt_new(name));
}

int xl_adt_remove(xl_adt_node *n, const char *name)
{
  for (unsigned i = 0; i < n->nchildren; i++) {
    if (strcmp(xl_adt_name(n->children[i]), name)) continue;
    xl_adt_free(n->children[i]);
    memmove(&n->children[i], &n->children[i + 1], (n->nchildren - i - 1) * sizeof *n->children);
    n->nchildren--;
    return 0;
  }
  return -1;
}

static xl_adt_node *walk(xl_adt_node *root, const char *path, int create)
{
  xl_adt_node *n = root;
  while (n && *path) {
    char part[64];
    size_t len = strcspn(path, "/");
    if (len >= sizeof part) return NULL;
    memcpy(part, path, len);
    part[len] = 0;
    path += len + (path[len] == '/');
    if (!len) continue;
    n = create ? xl_adt_add(n, part) : xl_adt_child(n, part);
  }
  return n;
}

xl_adt_node *xl_adt_find(const xl_adt_node *root, const char *path) { return walk((xl_adt_node *)root, path, 0); }
xl_adt_node *xl_adt_path(xl_adt_node *root, const char *path) { return walk(root, path, 1); }

size_t xl_adt_flat_size(const xl_adt_node *n)
{
  size_t size = 8;
  for (unsigned i = 0; i < n->nprops; i++) size += 36 + ((n->props[i].length + 3u) & ~3u);
  for (unsigned i = 0; i < n->nchildren; i++) size += xl_adt_flat_size(n->children[i]);
  return size;
}

static uint8_t *flatten(const xl_adt_node *n, uint8_t *out)
{
  uint32_t head[2] = { n->nprops, n->nchildren };
  memcpy(out, head, 8);
  out += 8;
  for (unsigned i = 0; i < n->nprops; i++) {
    const xl_adt_prop *p = &n->props[i];
    uint32_t length = p->length | (p->flags & 1 ? 0x80000000u : 0);
    size_t padded = (p->length + 3u) & ~3u;
    memset(out, 0, 36 + padded);
    memcpy(out, p->name, strlen(p->name));
    memcpy(out + 32, &length, 4);
    if (p->length) memcpy(out + 36, p->value, p->length);
    out += 36 + padded;
  }
  for (unsigned i = 0; i < n->nchildren; i++) out = flatten(n->children[i], out);
  return out;
}

void xl_adt_flatten(const xl_adt_node *root, uint8_t *out) { flatten(root, out); }

static xl_adt_node *parse(const uint8_t *data, size_t size, size_t *offset, unsigned depth, const char **error)
{
  if (depth > 64 || size - *offset < 8) { *error = "device tree node truncated or too deep"; return NULL; }
  uint32_t nprops, nchildren;
  memcpy(&nprops, data + *offset, 4);
  memcpy(&nchildren, data + *offset + 4, 4);
  *offset += 8;
  if (nprops > (size - *offset) / 36) { *error = "device tree property count runs past the tree"; return NULL; }
  xl_adt_node *n = xmalloc(sizeof *n);
  for (uint32_t i = 0; i < nprops; i++) {
    if (size - *offset < 36 || !memchr(data + *offset, 0, 32)) { *error = "bad device tree property header"; goto bad; }
    uint32_t raw;
    memcpy(&raw, data + *offset + 32, 4);
    uint32_t length = raw & 0x7fffffff;
    const char *name = (const char *)(data + *offset);
    *offset += 36;
    if (length > size - *offset || (((size_t)length + 3) & ~(size_t)3) > size - *offset) { *error = "device tree property value runs past the tree"; goto bad; }
    if (xl_adt_get(n, name)) { *error = "device tree property repeated in a node"; goto bad; }
    xl_adt_set(n, name, data + *offset, length);
    n->props[n->nprops - 1].flags = raw >> 31;
    *offset += (length + 3u) & ~3u;
  }
  if (nchildren > (size - *offset) / 8) { *error = "device tree child count runs past the tree"; goto bad; }
  for (uint32_t i = 0; i < nchildren; i++) {
    xl_adt_node *c = parse(data, size, offset, depth + 1, error);
    if (!c) goto bad;
    attach(n, c);
  }
  return n;
bad:
  xl_adt_free(n);
  return NULL;
}

xl_adt_node *xl_adt_parse(const uint8_t *data, size_t size, const char **error)
{
  const char *ignored;
  size_t offset = 0;
  if (!error) error = &ignored;
  *error = NULL;
  xl_adt_node *root = parse(data, size, &offset, 0, error);
  if (root && offset != size) {
    *error = "bytes after the device tree";
    xl_adt_free(root);
    return NULL;
  }
  return root;
}

void xl_adt_dump(const xl_adt_node *n, FILE *out, int depth)
{
  fprintf(out, "%*s%s {\n", depth * 2, "", xl_adt_name(n));
  for (unsigned i = 0; i < n->nprops; i++) {
    const xl_adt_prop *p = &n->props[i];
    if (!strcmp(p->name, "name")) continue;
    fprintf(out, "%*s%s%s = ", depth * 2 + 2, "", p->name, p->flags & 1 ? " [placeholder]" : "");
    int text = p->length > 1 && p->value[p->length - 1] == 0;
    for (uint32_t b = 0; text && b + 1 < p->length; b++)
      if (p->value[b] && (p->value[b] < 32 || p->value[b] > 126)) text = 0;
    if (text) {
      fputc('"', out);
      for (uint32_t b = 0; b + 1 < p->length; b++) fputc(p->value[b] ? p->value[b] : '|', out);
      fputs("\"\n", out);
    } else if (p->length == 4) {
      uint32_t v; memcpy(&v, p->value, 4); fprintf(out, "0x%x\n", v);
    } else if (p->length && p->length % 8 == 0 && p->length <= 64) {
      fputc('<', out);
      for (uint32_t b = 0; b < p->length; b += 8) { uint64_t v; memcpy(&v, p->value + b, 8); fprintf(out, "%s0x%llx", b ? " " : "", (unsigned long long)v); }
      fputs(">\n", out);
    } else {
      for (uint32_t b = 0; b < p->length && b < 32; b++) fprintf(out, "%02x", p->value[b]);
      fprintf(out, "%s (%u bytes)\n", p->length > 32 ? "..." : "", p->length);
    }
  }
  for (unsigned i = 0; i < n->nchildren; i++) xl_adt_dump(n->children[i], out, depth + 1);
  fprintf(out, "%*s}\n", depth * 2, "");
}
