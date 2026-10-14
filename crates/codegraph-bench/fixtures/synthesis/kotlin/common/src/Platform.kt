package app

expect fun platformName(): String

fun greeting(): String = "Hello " + platformName()
