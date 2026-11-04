typedef void opcode_t(unsigned char op);

static void op_nop(unsigned char op) {}
static void op_inc(unsigned char op) {}

static opcode_t *opcodes[2] = { op_nop, op_inc };

void step(unsigned char op) {
    opcodes[op](op);
}
