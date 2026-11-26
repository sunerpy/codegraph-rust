package gin

func Logger(c *Context) {}

func getUser(c *Context) {}

func setup(e *Engine) {
	e.Use(Logger)
	e.GET("/users", getUser)
	e.GET("/inline", func(c *Context) {})
}
