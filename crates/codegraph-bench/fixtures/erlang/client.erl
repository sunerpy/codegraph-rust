-module(client).
-export([fetch/1, broken/1, mapper/1]).

fetch(Key) ->
    store:get(Key, nil).

broken(Key) ->
    store:get(Key, nil, extra).

mapper(Values) ->
    lists:map(fun store:get/1, Values).
