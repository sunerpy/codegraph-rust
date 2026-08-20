union Value { int i; float f; };
struct Packet { int id; };
void Value() { }
void ctor_union()  { Value v{1};  (void)v; }
void ctor_struct() { Packet p{2}; (void)p; }
