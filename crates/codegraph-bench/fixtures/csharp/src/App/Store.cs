using System;
using System.IO;
using static System.Math;
using App.Support;

namespace App
{
    public interface IStore
    {
        void Save(int x);
    }

    public class BaseStore
    {
        protected int Seed() { return 1; }
    }

    // base_list: one class base plus one interface.
    public class Store : BaseStore, IStore
    {
        private readonly TextWriter _innerWriter;
        private int seed = Seed();
        private readonly TypeMap map = new();

        public Store(TextWriter writer) { _innerWriter = writer; }

        public int Count { get { return Measure(); } }
        public int Size => Measure();
        public TypeMap Map { get; set; }

        public void Save(int x)
        {
            _innerWriter.WriteLine(x);
            Log("saved");
            var squared = Pow(x, 2);
        }

        public void Log(string message) => Log(message, 0);
        public void Log(string message, int level) { Flush(); }

        private int Measure() { return seed; }
        private void Flush() { }
    }

    public class User
    {
        public Store Make(Store s)
        {
            TypeMap t = new TypeMap();
            var store = new Store(Console.Out);
            store.Save(1);
            return store;
        }
    }
}
