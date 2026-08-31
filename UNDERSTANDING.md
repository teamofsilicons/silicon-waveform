# UNDERSTANIDNG.md - waveform

This is the understanding.md for waveform, waveform is our Text to Speech (TTS) and Speech to Text (STT) provider. Waveform can be used by any of our apps to do their TTS and STT.

This will be used by both our carbons and silicons.

# Glossary

`Carbon` - The human in the system. Every human account is called a carbon.
`Silicon` - Our AI Agent (silicon) account is refered to as a Silicon.
`Org` - This is our organisation, this is where all the silicons and carbons would stay for a single organisation and defines the scope. 


# TTS

For our text to speech service we use the following services:

Gemini 3.1 Flash TTS Preview as our first option
then ElevenLabs Multilingual v2 as our fallback model
then OpenAI `tts-1` as our final fallback model


### Request

In the request it would be possible to include `text and lang`. That's all. Text is required, lang is optional. `lang` is passed only to Gemini TTS. 



# STT

For our speech to text service we use the following services:

Gemini 3.5 Transcribe as our first option
gpt-transcribe as our fallback model 
Deepgram Nova-3 Multilingual as our final fallback model


### Request

Request would just include the file_url(this must be the uploaded link) and language. Similarly `language` is optional. 


# How login works

Logging in and signing up are handled entirely by Silicon IAm (this is our access and authorization management layer). You would have an app_id and app_secret stored in your env that you can use to request the login and signup from Silicon IAm (read [[../silicon-iam/UNDERSTANDING.md]]) you would realise how you would need to login and singup using silicon IAm. For both signing in and signing up into the system would need Silicon IAm authorization, once you have the access token from SIlicon IAm for the user logged in, render the application accordingly. 

The webhook endpoint you have would give you information whenever someone logs out, kicked from org, anything changes you would know.


# How it works

For all the requests, it would be syncronous and would need to hold the connection for the generation to take place. For each request it would be attached with a silicon or carbon id, for that silicon or carbon 

# How other apps would use waveform

For other apps configured in IAm they should also be able to use waveform, for that the app would send a request to you to perform as a specific carbon or silicon user, for the said request it would send you app_id and an proof_token. You can send an request to IAm to verify this proof_token by sending it the app_id and the proof_token if it verifies let the application perform the requested action, otherwise deny it. Until the verification is held keep the request alive.  

These authenticated apps should be able to perform all actions on behalf of the user. Except for the delete actions for the files they haven't created. 


# How to use other apps

For using any other application, you can send a request to them with a proof_token and app_id. The proof_token is created using carbon's/silicon's auth+app_secret and then hash it. This proof_token is used to let you perform actions as the giver carbon or silicon.


# How waveform will use other applications

You will be converting the recieved tts response into mp3 and store it inside Silicon Briefcase this is to be created by the silicon. For the file naming keep it `tts_{date}_{time}`. 

Take a read at [../silicon-briefcase/UNDERSTANDING.md/] on how to use briefcase.