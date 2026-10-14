namespace Outer
{
    namespace Inner
    {
        public class Deep
        {
            public void Dive() { Surface(); }
            public void Surface() { }
        }
    }

    public class Shallow
    {
        public void Call() { new Inner.Deep().Dive(); }
    }
}

namespace JetBrains.Annotations
{
    public class Guard { }
}

namespace Second
{
    public class Two { }
}
