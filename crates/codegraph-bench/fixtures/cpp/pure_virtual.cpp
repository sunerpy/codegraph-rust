class AbstractStore {
public:
    virtual int read(int key) = 0;
    virtual AbstractStore *clone() = 0;
    virtual AbstractStore &operator=(const AbstractStore &) = 0;
    int inherited_pure() = 0;
    int declaration_only(int key = 0);
    int (*callback)(int) = 0;
    int data = 0;
};

class DiskStore : public AbstractStore {
public:
    int read(int key) override { return key; }
    AbstractStore *clone() override { return this; }
};

int fetch_store(AbstractStore *store, int key)
{
    return store->read(key);
}
