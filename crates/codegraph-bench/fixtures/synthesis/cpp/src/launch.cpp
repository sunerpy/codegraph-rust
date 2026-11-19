void kernel_a(int n) {}

template <typename T>
void kernel_b(T n) {}

void launch(int n) {
    auto kernel = &kernel_a;
    if (n > 1) {
        kernel = &kernel_b<int>;
    }
    kernel(n);
}
