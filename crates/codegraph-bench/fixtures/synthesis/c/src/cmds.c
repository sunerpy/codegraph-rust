#include "cmds.h"

static void getCommand(int argc) {}
static void setCommand(int argc) {}

static struct COMMAND_T commandTable[] = {
    MAKE_CMD("get", getCommand),
    MAKE_CMD("set", setCommand),
};

void call(struct client *c, int argc) {
    c->cmd->proc(argc);
}
