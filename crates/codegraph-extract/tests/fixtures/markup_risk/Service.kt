package com.example.demo

import com.example.db.Db

interface Service {
    fun run(): Entity
}

enum class Level { LOW, HIGH }

class Repo(private val db: Db) {
    val name: String = "repo"

    suspend fun fetch(id: String): Entity? {
        return db.query(id)
    }
}

class Outer {
    enum class Inner { A }

    fun touch() {
        helper()
    }
}

object Registry {
    fun lookup(): Repo = Repo(Db())
}

typealias EntityId = String

fun String.shout(): String = this.uppercase()

expect fun platform(): String

val topLevel = Registry.lookup()
