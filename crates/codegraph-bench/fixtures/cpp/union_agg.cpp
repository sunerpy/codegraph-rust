union WithMethod {
    int i; float f;
    int read_field() { return i; }
};
struct SWithMethod {
    int i;
    int read_field() { return i; }
};
void drive_union()  { WithMethod w;  w.read_field(); }
void drive_struct() { SWithMethod s; s.read_field(); }
