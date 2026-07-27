template <typename T>
class Box {
public:
    T get() const;
    void set(T v);

private:
    T value;
};

template <typename T>
T Box<T>::get() const {
    return value;
}

template <typename T>
void Box<T>::set(T v) {
    value = v;
}
