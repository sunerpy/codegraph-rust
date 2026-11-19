package rpc

type server struct {
	UnimplementedGreeterServer
}

func (s *server) SayHello(name string) (string, error) {
	return "hello " + name, nil
}
