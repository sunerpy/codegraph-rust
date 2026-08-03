

fun greet(name: String, times: Int = 1): String = name

fun inferred(flag: Boolean) = flag

fun <T> identity(value: T): T = value

class Processor(val id: Int) {
    suspend fun process(
        input: Map<String, Int>,
        retries: Int = 1,
    ): Result<String>? = null
}

fun String.decorate(prefix: String): String = prefix + this
