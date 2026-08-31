-module(multi).
-export([job/1, job/2]).

job(Value) ->
    Value.

job(Left, Right) ->
    {Left, Right}.
