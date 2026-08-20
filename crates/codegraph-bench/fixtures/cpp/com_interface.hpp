struct SControl : IBase {
    virtual void Ping() = 0;
    void Run() { }
};

interface IWidget : IBase {
    virtual void Ping() = 0;
    void Run() { }
};

interface IPlain {
    virtual void Ping() = 0;
};

interface INewlineBrace
{
    virtual void Ping() = 0;
};

int interface_count = 0;
