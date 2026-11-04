typedef void cmdProc(int argc);

struct command {
    const char *name;
    cmdProc *proc;
};

struct client {
    int id;
    struct command *cmd, *lastcmd;
};

#define MAKE_CMD(name, fn) { name, fn }
#define COMMAND_T command
