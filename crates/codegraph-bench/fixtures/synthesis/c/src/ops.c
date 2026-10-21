struct file_ops {
    int (*open)(const char *path);
    int (*close)(int fd);
};

static int my_open(const char *path) { return path != 0; }
static int my_close(int fd) { return fd; }

static const struct file_ops ops = {
    .open = my_open,
    .close = my_close,
};

int run(const char *path) {
    int fd = ops.open(path);
    return ops.close(fd);
}
