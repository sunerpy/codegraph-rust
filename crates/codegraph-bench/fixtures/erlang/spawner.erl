-module(spawner).
-export([go/1]).

go(Args) ->
    erlang:spawn(single, work, Args),
    erlang:spawn(multi, job, Args).
