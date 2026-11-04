typedef int opRun(void);

struct op {
    const char *name;
    opRun *run;
};

#define MK(nm, fn) nm, fn
#define CMD_T op

static int optab_a(void) { return 0; }
static int optab_b(void) { return 0; }

#include "optab.def"

int dispatch_op(struct op *o) {
    return o->run();
}
