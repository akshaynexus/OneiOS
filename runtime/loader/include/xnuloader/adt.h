// SPDX-License-Identifier: MIT
// Apple device tree (ADT) in memory: build, parse, edit, flatten.
// Format (as XNU's pexpert/gen/device_tree.c reads it; the same layout iOSPlus10's
// external/slop/tools/adt.py and UniXNU's make-*-adt.py scripts write):
//   node = { u32 nprops, u32 nchildren, props..., children... }
//   prop = { char name[32], u32 length (bit 31: placeholder flag), value padded to 4 bytes }
// Every node's first property is "name".
#ifndef XNULOADER_ADT_H
#define XNULOADER_ADT_H

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#define XL_ADT_NAME_MAX 32      // property name field, NUL padded (31 characters at most)

typedef struct xl_adt_prop {
  char     name[XL_ADT_NAME_MAX];
  uint32_t length;
  uint32_t flags;               // bit 0: the placeholder flag of the flat form
  uint8_t *value;
} xl_adt_prop;

typedef struct xl_adt_node {
  xl_adt_prop         *props;
  unsigned             nprops;
  struct xl_adt_node **children;
  unsigned             nchildren;
} xl_adt_node;

xl_adt_node *xl_adt_new(const char *name);
void         xl_adt_free(xl_adt_node *node);
const char  *xl_adt_name(const xl_adt_node *node);

// Direct child by name, or NULL. xl_adt_add finds or creates it.
xl_adt_node *xl_adt_child(const xl_adt_node *node, const char *name);
xl_adt_node *xl_adt_add(xl_adt_node *node, const char *name);
// Removes and frees a direct child; returns 0, or -1 if there is none.
int          xl_adt_remove(xl_adt_node *node, const char *name);
// Node at a '/'-separated path below `root` ("chosen/memory-map"), or NULL. xl_adt_path creates it.
xl_adt_node *xl_adt_find(const xl_adt_node *root, const char *path);
xl_adt_node *xl_adt_path(xl_adt_node *root, const char *path);

// Sets (replaces or appends) a property. Returns 0, or -1 for a name longer than 31 characters.
int  xl_adt_set(xl_adt_node *node, const char *name, const void *value, size_t length);
int  xl_adt_set_u32(xl_adt_node *node, const char *name, uint32_t value);
int  xl_adt_set_u64(xl_adt_node *node, const char *name, uint64_t value);
int  xl_adt_set_str(xl_adt_node *node, const char *name, const char *value);   // with its NUL
int  xl_adt_unset(xl_adt_node *node, const char *name);
const xl_adt_prop *xl_adt_get(const xl_adt_node *node, const char *name);

size_t xl_adt_flat_size(const xl_adt_node *root);
// Writes exactly xl_adt_flat_size(root) bytes.
void   xl_adt_flatten(const xl_adt_node *root, uint8_t *out);
// Parses a flat tree that must fill `size` exactly. NULL with *error set if it does not.
xl_adt_node *xl_adt_parse(const uint8_t *data, size_t size, const char **error);
void   xl_adt_dump(const xl_adt_node *node, FILE *out, int depth);

#endif
