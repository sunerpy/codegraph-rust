package gin

type HandlerFunc func(*Context)

type HandlersChain []HandlerFunc

type Context struct {
	handlers HandlersChain
	index    int
}

// Next runs the rest of the handler chain.
func (c *Context) Next() {
	for c.index < len(c.handlers) {
		c.handlers[c.index](c)
		c.index++
	}
}

type Engine struct {
	Handlers HandlersChain
}

func (e *Engine) Use(middleware ...HandlerFunc) {
	e.Handlers = append(e.Handlers, middleware...)
}

func (e *Engine) GET(path string, handlers ...HandlerFunc) {}
