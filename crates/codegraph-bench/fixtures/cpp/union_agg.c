union Named { int i; float f; };
struct Ctl { int x; };
typedef union { int a; float b; } AnonU;
typedef struct { int a; int b; } AnonS;
typedef union NamedTag { int c; } NamedU;
union Fwd;
struct FwdS;
union { int q; } anon_var;
int read_named(union Named *p) { return p->i; }
