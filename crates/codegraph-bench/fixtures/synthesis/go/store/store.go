package store

type Getter interface {
	Get(key string) string
}

type Store interface {
	Getter
	Set(key, value string)
}

type base struct{}

func (base) Get(key string) string { return key }

type MemStore struct {
	base
	data map[string]string
}

func (m *MemStore) Set(key, value string) { m.data[key] = value }

type Alias = MemStore

type Defined MemStore

func Use(s Store) string {
	s.Set("a", "b")
	return s.Get("a")
}
