// Stand-in declarations for the syntax check (see CoreFoundation/CoreFoundation.h beside this).
#ifndef STUB_OBJC_RUNTIME_H
#define STUB_OBJC_RUNTIME_H
typedef struct stub_object *id;
typedef struct stub_class *Class;
typedef struct stub_selector *SEL;
typedef struct stub_method *Method;
typedef void (*IMP)(void);
Class objc_getClass(const char *name);
SEL sel_registerName(const char *name);
Method class_getInstanceMethod(Class cls, SEL name);
Method class_getClassMethod(Class cls, SEL name);
IMP method_setImplementation(Method method, IMP implementation);
#endif
