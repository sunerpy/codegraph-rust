/*
 interface INegComment;
*/
interface class INegCli { public: virtual void B()=0; };
interface struct INegStructKw { int x; };
const char* idl = R"(
interface INegGhost { virtual void B() = 0; };
)";
#define NEG_DECL \
  interface INegMacro
struct NegReal { int x; };
