namespace fixture {
const char *source_template = u8R"MARKUP(
DECLARE_THING(
struct Ghost { int value; };
UE_DEPRECATED(
class GHOST_API IgnoredApi {};
float4 position [[position]];
__global__ void ignored_kernel() {}
ignored_kernel<<<1, 1>>>();
)MARKUP";
}

int after_raw_string(int value)
{
    return value;
}

int final_after_raw_string(void)
{
    return 1;
}
