__global__ void add_kernel(int* out, const int* a, const int* b) {
    int i = 0;
    out[i] = a[i] + b[i];
}

template <typename T, int N>
__global__ void scale_kernel(T* x, T factor) {
    x[0] = x[0] * factor;
}

DEFINE_FLASH_FORWARD_KERNEL(my_kernel, int n) {
    int i = n;
}

void launch(int* out, const int* a, const int* b, float* data) {
    add_kernel<<<grid, block>>>(out, a, b);
    scale_kernel<float, 256><<<grid, block>>>(data, 2.0f);
}
