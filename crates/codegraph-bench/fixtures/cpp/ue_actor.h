#pragma once

class ENGINE_API UFoo : public UObject
{
    GENERATED_BODY()
    UPROPERTY(EditAnywhere)
    int Health;
    ENGINE_API virtual void Tick();
    UFUNCTION()
    void Bar();
};
