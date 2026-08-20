union Reg { int raw; float val; };
struct Ctl { int id; };
void mk_union()  { Reg r = Reg();  (void)r; }
void mk_struct() { Ctl c = Ctl();  (void)c; }
