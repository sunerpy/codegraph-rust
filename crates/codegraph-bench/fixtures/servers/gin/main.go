package main

import "github.com/gin-gonic/gin"

func ping(c *gin.Context) {
	c.JSON(200, gin.H{"message": pong()})
}

func pong() string {
	return "pong"
}

func main() {
	r := gin.Default()
	r.GET("/ping", ping)
	_ = r.Run()
}
