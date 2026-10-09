# Transcript: Theo — "Jev is incredible" (YouTube F3YXg7AaKWE)

- source: https://www.youtube.com/watch?v=F3YXg7AaKWE&t=912s
- title: Jev is incredible
- uploader: Theo - t3․gg
- upload_date: 20260921
- duration_s: 1829
- url: https://www.youtube.com/watch?v=F3YXg7AaKWE
- audio: /tmp/claude-1000/-home-beckett-projects-tiro-mp3-upload/b8feb59e-91eb-47ae-b5d1-2368786b0240/scratchpad/out-full/F3YXg7AaKWE.mp3
- decoded /tmp/claude-1000/-home-beckett-projects-tiro-mp3-upload/b8feb59e-91eb-47ae-b5d1-2368786b0240/scratchpad/out-full/F3YXg7AaKWE.mp3 in 3.0s: 29268992 samples @ 16 kHz (1829.3 s of audio)
- model large-v3 (int8) on gpu, beam 5, vocab prompt: (none)
- GPU worker ready (device 1) in 3.5s
- transcribed 1829.3 s of audio in 513.1 s (3.56x realtime), 35576 chars
- wall clock: 517s
- generated: 2026-09-23 13:23 EDT by scripts/transcribe-file.sh

## Text

Oh boy, it's new model time.

This one's very different though.

This isn't our usual new LLM that's slightly better at code.

And it's not going to be the type of thing that the model counter guy is gonna show up and say, look, best new model.

If anything, this might take the counter guy out of his job because this model is for data processing.

It's by a company called TypeSafe AI, and it's called Jev.

The point of this model isn't to generate text, write code, or do all the things that we expect models to do today.

It is classification.

This model is the best model ever made to take data and organize it and classify it.

It returns JSON perfectly in a TypeSafe format.

When you give it a format, it does it, and it does it well.

That means this model isn't going to replace things like Fable or Astra.

Well, hopefully not.

If you are using Fable and Astra for the stuff you can do with this, I have a lot of questions for you.

All that said, this model is incredibly capable, and that's why it's blown up on Twitter.

I've seen demos of everything from lightning fast computer use, even in like the iOS simulator, to the model playing Minecraft at lightning speed, to compaction, that takes under a second instead of multiple minutes.

I can't wait to show you all the best things you can do with this model, but first we got to classify something.

The next section, which is the sponsor break.

If you're not a developer, you can skip this ad, but if you are, you should listen close.

Have you ever sent a prompt to CloudCode, Codex, Cursor, or some other tool and been surprised that it took hours when you expected it to take minutes?

Did you check to see why?

Because I would bet, good money, there's a very good chance that a handful of things happened.

Maybe it took too long to download the Docker image.

Maybe it spun up and had some error when it did.

Maybe the CI that it was trying to run took forever.

Maybe it pushed it up to GitHub and the CI run on there was going to take hours long.

I cannot tell you how many times I've run into this.

Wouldn't it be nice if your agents could do all of those things way faster and more reliably, and when those things failed, you got insight into it?

Imagine taking all of these eight-minute builds and knocking them down to 20 seconds, or building and testing three React apps and seven Go binaries in 30 seconds, or making your cross-platform builds 10 to 20 times faster.

Hopefully you get the idea now because today's sponsor is Depot and they are trying their damnedest to make AI more capable, not by giving it things it can't do already, but by making all of the things it does do more reliable and faster.

Whether you're looking for a faster and cheaper alternative to GitHub Actions or just a nice place to let your agents run or a cache for all the Docker images you and your team are using, Depot provides all of this and more.

Their CI platform is fully compatible with GitHub Actions, which means you can swap over trivially, but if you don't want to deal with GitHub's downtime, you can move fully over to Depot's platform, still compatible with Actions, but without having to worry about GitHub's reliability You can also transform those workflows to let them run in parallel, which ends up being comically faster than the alternatives on GitHub.

Their CLI is incredible too.

You and your agents will understand it immediately.

It makes it trivial to migrate your CI, run it without pushing up code, diagnose things when they go wrong, find secrets and clone them across different places, and more.

You'd even SSH into a session while it is running to figure out what's going wrong in real time.

And when I say you, obviously, I mean your agents.

GitHub Actions were poorly assembled for humans over 10 years ago.

Depot was crafted carefully for agents today.

Figure out what that means at soydiv.link slash depot.

Quick breakdown on how this all happened because it's actually been pretty crazy to watch.

It feels like it just poof appeared and took over my entire Twitter feed.

Like half of what I've been seeing is all of the stuff about Jev.

It was created by Diogo, who used to work at OpenAI.

He apparently helped co-invent ChatGPT as well as RLHF.

And as great as RLHF is for model behaviors, it is not helping as much with classification, which is the thing he cares about.

If you focus more on the classifying side, and less on the general usage of text generation side, what you can get is way, way faster and way, way cheaper.

In particular, all output tokens being free.

The cost characteristics of this model are almost as insane as the speed.

Both are crazy.

So let's read through the announcement.

The core concept here is system one models.

This is an idea that comes from Thinking Fast and Slow by Daniel Kahneman.

System one is the fast and intuitive judgment of your brain, the part of your brain that could do things without effort, that looks at somebody and is like, yeah, that shirt's blue.

System two is slow and deliberate reasoning.

In TypeSafe's framing, Jev handles the first kind of task while reasoning models still handle the second.

They named it Jev after Jevin's paradox, which I actually think is pretty cute.

The idea is that when the steam engine gets more efficient, it actually increased coal consumption, not decreased, because now that power's cheaper, we can use it for more things.

That is really the right way to think of this model.

It's not unlocking new capabilities that weren't possible before.

It's making them fast and cheap enough that they're way more viable than they've been before.

But what are those cases?

What is this valuable for?

Pretty much anything that needs to be classified.

Think organizing videos by their topic in my YouTube channel, figuring out if an email's important or not, and what category it should be put under.

Things like safety and moderation, identifying what messages are safe and which ones aren't, and what makes them unsafe.

There's so many things you can use it for, and when it's as insanely fast as this model is, the results are kind of crazy.

We'll go to the official announcement in a sec, but first I just want to show you how insanely fast the model is with an example of something it can do.

I'm not saying it can do it well.

You'll actually see the type 1, type 2 distinction here.

But this is a game of checkers that I built to use Jev.

Jev takes the whole board as an input, as a state, not an image because it doesn't have vision.

It takes the board state as data, and then it chooses what to move based on the output it gives.

So here it might say it wants to move B6 to C5.

I go first, so I'll make a move.

Watch how fast it responds.

I am clicking now.

Yeah, it's practically instant.

There is a catch though.

It's not very smart.

I was able to barely pay attention while playing and crush this model.

Yeah, it's just, it's not good.

I was barely paying attention and I'm going to crush it here.

The point I'm trying to make is that it's super fast, but it's not using the reasoning part of its brain, so to speak.

It is just processing the data.

Let's take a look at what Diogo had to say.

Models have been superhuman at chat for years, so where's all the automation?

This has been my driving question for the last four years.

At OpenAI, I helped build the methods that make language models useful at following instructions and talking with people.

The work ended up as the research behind ChatsGPT.

At the time, I thought maybe chat models would lead to AGI, but despite the hype, it became obvious to me that there was something really big missing.

After two years in stealth, countless technical challenges and research breakthroughs, he's beyond excited to announce that today TypeSafe AI is releasing its first System 1 model, a new class of frontier model built to make fast, structured decisions that software can use directly.

This is one of the biggest important pieces to understand.

The point of this model is to integrate it into your code.

This model isn't even useful if a human triggers it.

The model is useful if the human triggers some code to run or something else triggers some code to run.

And when the code runs, it passes certain data to this model and then it comes back with JSON.

That's when it's useful, when it's integrated almost like a function in your code base.

In order to do this, they had to build a new stack entirely focused on automation, new model architectures, parallel samplers for maximum efficiency, and training methods that they call reinforcement learning for calibrated decisions.

The first public version of this is Jeb, which is now available in early access.

It is invite-only, but you can get access to it on things like OpenRoute or the Vercel AI Gateway and a couple other places.

While Jeb gives up string generation, it's optimized for structural outputs and it can't hallucinate.

This is a key piece.

Since it has to give the output in a certain format, it's not going to change or make up the format, which happens a hilarious amount.

Think of Jeb as a frontier intelligence function call.

Unstructured state in, typed probabilistic decisions out.

That's the key piece.

You hand it some bullshit text data and you get back a type-safe shape.

There have been other attempts to do this in various different ways, from attempts to train models to act like this, to tools to force other models to behave similarly too.

One that I really liked once it clicked for me is called BAML.

The point of BAML was to make a language that interfaces between agents and real code so that you can define something in a syntax that, of course, agents understand, but also to give a specific format and instruction set to the model to get it to output a certain shape.

They frame it kind of like TypeScript, but as the interface from the TypeScript to the LLM.

For example, here is a text sentiment classifier that they have on their site.

You give it a label type, which positive, negative, or neutral are valid for, a verdict, which has label and confidence, which is a float, and you define the function classify.

It takes in text.

It outputs a verdict.

You use OpenAI, GBT 5.5 here as the client.

You hand it the prompt.

You use their special formatting things, and then you can call this in your TypeScript code in order to get a formatted correctly output.

BAML is basically a made-up language, but what makes it cool is how well it interfaces with other languages and how it fixes all the things that can go wrong.

For example, I was using this a bunch with GBT OSS 120b to find comments that mentioned sponsors and things, and when I did that, I had a really good experience with it.

It all just worked, and when I tried to move off BAML for another thing I was doing, I learned that apparently GBT OSS 120b doesn't format JSON correctly half the time.

One of the many things they do with BAML is in their runtime, they fix the malformatting in the JSON outputs to make sure it is guaranteed to match the format it puts on the tin.

When you say this is the response format and you call it through BAML, that's the format you get.

Sometimes that takes time, though, because it has to reformat it, fix it, change it, all with code that it's running, and even some high-end models can sometimes screw these things up up.

Jev literally can't.

If you give it a JSON format, you get back that JSON format.

It doesn't handle things like a message history while it's input.

It's really looking for structured program state, like data it can use to make decisions.

I already see people getting confused, so I want to be really, really clear here.

The point of things like BAML, like Jev, and like all structured outputs is because LLMs are not deterministic.

If I have code that formats someone's name, it will always do the same thing when I call it with a given name.

If I have an LLM that I ask to format the name, it might do something else.

If I have a function that returns a user object that has a name, colon, string, and an age, colon, number, and I write code to get that, it will always be that shape.

If I call an LLM, it might screw up the formatting of the name.

It might use a float for age instead of an int.

There's a lot of different things that can go wrong.

The point of Jev is that you can call it as reliably as you call code.

And it's really fast, kind of like code.

It also can do this for a bunch of inputs at once.

It can do that once in parallel, which is super, super cool.

And the token cost is insanely low.

It's about 4 cents per million tokens in compared to $10 per million on a model like Fable.

I see more confusion in chat already.

So is it deterministic?

The format of the output is yes.

There are two ways things can be deterministic.

The shape and the content.

The content isn't deterministic, but it should be quite reliable.

The actual format is what is deterministic.

It will always give you the format that you define.

And on the topic of cost, the tokens out are free because it's so cheap.

They frame it as too cheap to meter.

Meanwhile, with LLMs, the output tokens are so expensive.

It's often the majority cost when you're doing stuff like this.

Traditional LLMs can take three to over 300 seconds to do the type of classification work that this model can do in 70 to 500 milliseconds.

That is actually 40 to 200x faster.

There's no doubt on that claim.

That is real and true and based.

Even if prompted for a confidence estimate, models tend to be overconfident and inconsistent.

The model can do a task 95% of the time but doesn't say when it's in the 5%.

You can't automate the task.

Meanwhile, Jev will always communicate confidence and uncertainty with every output.

So you can calibrate around the accuracy numbers you get and the answers also tend to be more consistent.

I like the framing here of one of the good use cases being a smart if statement.

I know a lot of y'all don't code anymore, sadly.

I get it.

But if statements were a great way of thinking of logic and that's what this model is for as a thing you put between states to decide what path you go down or to map reduce over a giant pile of data in order to get insights out of it.

And this is one of the coolest things you can do with it.

Real-time applications.

Not that like it'll build the app but as a tool in the app because it responds in just under 500 milliseconds worst case you can do something silly like a search with it or the demo I just showed with chess or checkers.

This demo for Matt is really cool as well where you can define a term and ask it to generate a color palette effectively for it and it shifts that bar at the bottom and it's practically real-time because the model is so quick to respond.

The final use case they have in their example list here is verifying everything.

Score, judge, verify, guardrail, and detect jailbreaks of LLM prompts reasoning traces and outputs.

Stuff like that.

I want to make a quick point though because one of the questions I've seen the most by far is something along the lines of wait, so if I use four different LLMs to generate an output I could use this to judge it.

You do understand how expensive it would be to generate all four of those and how silly it would be to have a model that's only using one side of its brain judge that?

The benefit of other LLMs is that they can reason.

They can think through the decision.

They can walk through your code base using agentic tools in order to figure out what changes to make.

They can grow their own context over time and prompt themselves with sub-agents.

They can do all of these different things.

It makes no sense at all to give a model that takes a bit of text input and immediately responds with JSON three different implementations of something by three different LLMs.

It just doesn't know enough to make a good decision there.

I want to beat into your guys' heads how this works and I'm sorry for those who get it because it's going to be tedious but I'm increasingly tired of comment sections that fundamentally don't understand what's being talked about.

This is a model that works like a switch statement.

It is roughly as intelligent as a switch statement.

It's for classifying things.

It doesn't know how to look through a code base to make a good decision.

It doesn't have the intelligence to distinguish between the outputs of language models.

And most importantly, the context wind is tiny.

It's only 32k tokens.

That doesn't mean it's not cool and it is cool as hell.

The speed and the cost is insane.

They do a comparison here against an LLM for responding to a query.

Typesafe race ask LLM versus Typesafe.

He got back the response immediately.

It was given 27 questions about a bunch of data and it responded to all of them with the right format with numbers ranking them and the type as well as confidence on its decisions for these things.

It took 0.114 seconds and it cost an amount that rounds to zero.

Meanwhile, Terra, once it finally finished, took 9 seconds and cost 1.3 cents.

That is a comical gap here.

170x cheaper and 70 something times faster.

Respect to them for keeping the cost per workflow in their chart in log, they could have made this linear and it would have looked hilarious.

The only models they measured that had better classification in their demo workflows than Jev were Sol and Opus 5.

They didn't test Fable or Astra but those are way too expensive.

You shouldn't even look at them for this.

But it is outranking DSV4 Flash in various tests for ranking things and it owns the Pareto Frontier with almost two orders of magnitude again because it's very specifically focused on this one thing.

They didn't publish the bench they used here but they gave some examples.

There's an alert.

The next step is a question.

Is this unauthorized activity with options for what it can choose?

Once that happens, we determine different flows it goes through and if we have decided that this is an incident, then we ask what state is the incident in and it has 11 readings for it.

It processes all the data.

We then ask what action it thinks we should take and then we rank the results after.

They are surprisingly transparent with a lot of things like they love to say all the things the model isn't good at.

This is where the numbers on their homepage come from.

The 193.6x faster and 444.6x cheaper but they say that they expect this is the higher end of real-world gains.

The context of these workflows were not deliberately chosen nor constructed to make our model look good.

Cool.

They're using the average of GPT-6 Astra and Fable 5.1 as the reference answer.

That's why they don't appear in the list and also why the data might not be perfect since those models might be wrong as well.

The LLMs use their System 1 LLM wrapper which constrains LLMs to output structured decisions compatible with their API.

We found it to be the most accurate way to get decisions from LLMs but this tends to be slower and more expensive than giving decisions without probabilities.

I find that they love to tie the idea of hallucination and type safety.

Type safety being you have a contract for what it should output and it follows the contract.

That's why they're type safe AI because the shape of the data will always be honored.

Hallucination goes a lot further than hallucinating fields in a return type but in this particular case it can be pretty brutal if your LLM hallucinates that it can change the fields when it can't.

That does suck for type safety.

The way they frame it here is that having a hallucinated tool call can be inconvenient for an agent but it's an absolute deal breaker if it's part of a system with latency guarantees or if it's buried several layers deep in a dependency chain.

Yep, that part I absolutely agree with.

No matter how smart an existing model is it can still hallucinate and have type errors.

They show that here where crazy enough Astra actually has more errors with structured tool outputs than Sol, Terra, and Luna.

It actually has more than Terra and Luna combined which is kind of crazy.

But then you start to look at models from a certain anthropic where Haiku had a 45.5% error rate.

That model is so bad and it needs to stop being used for anything.

It's about a year old now.

It might be over that.

Just, anthropic is treating Haiku as dead.

We should do them the favor of doing the same.

But of course, the number that matters here is Jev with a 0%.

This is for structured output errors.

When you give it a JSON format and tell it to honor it, Haiku cannot do it.

And we switch over to tool call error rates.

Funny enough, anthropic models start doing a lot better and open AI models start to have more problems.

But of course, Jev still is at 0.

It honors the format it's given.

They made a demo of it playing Doom and the engineer who made it was concerned about cost but they figured out even though it's running 10 times a second, the cost would still be under $7 an hour because of how efficient the model is and how cheap it is to run.

Remember, it doesn't have vision.

So this is all from game state data that it's being given.

The application state is handed to the model and it's given the JSON format on what it should decide to do next and it decides.

You will see some quirks with that though.

Watch how often it like just like rotates left and right wildly.

That's because every single frame it's told make a decision.

So it doesn't necessarily know what decision it made before.

It doesn't know it turned left so keep turning left.

So it's like, okay, looking here, I'll turn left.

Okay, new state, turn right.

On every single frame effectively, its brain is being wiped and it's making a new decision.

You get the idea though and I saw this with the chess as well.

It's not meant for long running jobs.

It's meant for making a quick decision on the fly or being part of some other long running jobs work.

They call out that this demo is again on structured state as a data structure with text, not on images yet.

This is particularly exciting.

This model having image support will be super, super useful.

The model is also insane at wiki racing because it makes decisions so quick.

So when it has the condesign HTML page and it knows where the links are on it, it can choose which one to click and continue going much faster.

Yeah.

It is very, very fast.

Terra even had a hallucination during its run.

That's funny.

Let's take a look at what people are using the model for.

They claim you can't use it for generating text, but if you give it the ability to respond by choosing which character it thinks makes the most sense, you can kind of get it to do things.

Here's one that I think actually makes way more sense from Chris over at Vercel.

It's the idea of using JSON render with Jev.

The point of JSON render is to make it easy to create a component and use it for arbitrary JSON so you can link the outputs of an LLM into your UI in a more visible and useful way.

But you have to wait for the LLM to generate the JSON before you can update the UI.

What if the model could do that comically faster?

That's a pretty big difference.

Yeah.

It's insane.

And it responds with the whole thing at once because it's not streaming in text.

Everybody's been telling me Ryan's been cooking with it.

Let's see what he's done.

1,500 of my own emails that I've exported from my email.

And we're going to run this classification model because I got access to it and see how well it runs the classification on it.

We're going to do a batch of 100 emails just to start out with eight workers and we'll see how long it takes to do it.

So let's start.

Boom.

100 emails done.

It does it.

It doesn't tell me how long it took.

Average was 200 milliseconds.

200 milliseconds per email.

P95 was 240 milliseconds and it did 38 per second, which is amazing.

So this is a much better example.

I already use LLMs to go through my email.

It's expensive, but it's fine.

This is a first pass to like get the quick, low effort spam things out of your way.

Super useful.

It's the things that are too cheap to justify running an LLM for or the things that are run too often to wait that much time for.

Once this has image support, an example of something we can do with this is take a bunch of frames for my video and ask it, does this frame have sensitive data or PII on it?

Like does it have an email address on it and to tell me where it is some amount so I can go find these and clean up our videos before we release them.

That type of thing is so nice.

I think computer use will also be way more compelling with it.

Once it has the ability to see what's going on, but it's already really good at navigating websites because it can just take the HTML and then decide what to do based on the current page content.

There's an example of it booking flights in under 7.1 seconds, right?

Yeah, that's crazy.

LLM is doing this take so much longer.

I am scared to even make this video from being real because if I increase excitement too much around this model, we're gonna end up with some really, really dumb things.

For example, this tweet from Braintrust, their goal is to increase observability for agents.

This is a really bad thing for them to tweet.

It makes me not trust them because they suggest that you should use Jev as a judge scorer in a tool like Braintrust to decide between outputs from LLMs.

They were generally suggesting that you replace an LLM that you use for judging for scoring agent responses with Jev.

And the reason is so that you don't need to spend time and resources prompting a general purpose model into an LLM judge.

What?

Those prompts take fucking 30 seconds to write, not even.

And like, if you just give it the examples from something like Jev, it's gonna follow them.

This is hilarious.

On that note, there was another demo that I actually think is really cool conceptually.

Like it looks crazy and it helps show what capabilities this model has.

But if you actually think it's a good idea to use a non-reasoning classifier model for compaction for your context, then I would put it in.

Then I would plead that you never, ever, ever stray from the defaults in those tools because you just fundamentally don't understand yet.

And don't worry, you're not the only one.

I don't think anyone has to.

Context compaction is a complex topic and it's gotten more complex over the years.

I will do my best to TL;DR why this is a bad idea, but you should just read my longer post if you're curious.

First thing, compaction isn't a filter.

We're not just going through your history and selectively deleting lines from it.

We are synthesizing a summary based on everything that's happened so far.

Jev also doesn't have enough context to even know what it's deciding on.

Then we just saw it doesn't have the tool call outputs and results.

It only has the inputs and a little bit of the context from the thread and not that much of it 'cause it's 32K tokens of context.

So it doesn't have enough data to make a decision, even separate from the fact that it doesn't have access to the reasoning data at all because the reasoning data is never shared by the labs anymore.

When you call the Claude or Codex APIs, you don't get back reasoning.

You might get back an encrypted payload that they can, then back to the reasoning on their end, or you might get a summary if you're lucky, but you don't know what the model was thinking when it made a decision.

So any attempt to compact that is not going to include those decisions.

This gets even worse when you remember that Anthropic is making changes to how history preservation works, such that if you edit your history, you lose all of the reasoning traces for that context.

There's also the fact that models are tuned on the way that they compact.

They do this in training now.

Models learn how to handle compaction well, and they make adjustments to the weights and how this works.

Through the process of training through RL, the compaction that the models do is already pretty damn good, and changing what history they have before they do it makes even less sense.

Another important thing to recognize is that cache writes are often, if not always for agentic use cases, much more expensive than cache reads end up being.

And when you remember how cache and validation works, you realize that this will probably break the cache quite a bit if you run it actively enough, because if you have a history like one, two, three, four, five, six, and then you delete number two, everything three onwards has to be rewritten because the history has to be prefixed.

Any changes early mean everything past that point's invalidated.

The model's given weird instructions to this implementation too.

Things like whatever's not kept is permanently deleted, but the assistant can rerun a tool if needed.

Some tools are destructive.

It's not that simple.

And you'll also get to a point if it's classifying in such a shallow way where it gets stuck in a loop where it's already removed everything it thinks doesn't matter, and it only has left what does.

No.

If people have actually tried this and benched it, it doesn't perform well at all.

There is one benefit to this style of compaction.

It's so fast that it fits in the attention span of the average Twitter user, so the video is guaranteed to go viral.

But you're not like the average Twitter user.

You've been watching this video for much longer than the attention span of the average Twitter user.

And for that, I appreciate you.

If you haven't hit the sub button, I would appreciate that as well, because a lot of y'all haven't.

And it seems like you want this type of long form content.

You should consider subscribing to signify that.

And as a Twitter user, trust me, you don't want to be like us.

Avoid it to the best of your ability.

I do think there are ways that a tool like Jev can be useful to things like agentic dev work, not having it write code or compact by context or change anything that the harness is doing.

The harnesses are pretty dang good now.

You should lean into the fact that billions upon billions of dollars are being spent on that and not reinvent the wheel constantly.

Don't get me started on the people who think they can compact their history by taking their entire context and then shoving it into a small image.

Insanity.

Anyways, a thing you can use this for is going through large amounts of data.

For example, all of your history using models.

Here are my 1118 T3 code chats that I did on this particular machine categorized and classified using Jev.

It classified 32,311 messages across 1118 threads and doing all of that cost 37 cents.

And it found that nearly half of what I did.

Was bug fixing and PR work specifically reviewing and managing PRS was 20% of my threads.

Remember that the results here are classifications.

So they're usually scored.

It's not responding with a list of strings on what to tag.

It's taking all of the values inside of the body there and giving you a threshold on a scale of zero to one for all of them.

And you can change what you want to count and not count.

For example, here with the simple web app I made, I am changing what bar we have.

Set for each of the classifications.

So if we have it at an 80% confidence interval, then 22% of my threads are expanding scope.

But if I bumped that up to 90%, it's down to 6.8 because it was not sure in the rest of those cases or as sure, so to speak funny enough, when I had Astra build this, it did note some of the things that it wasn't great at particularly trying to identify which threads could be used for content for me.

So one of the ideas it had was what if we can figure out which threads Theo should see?

How many threads would it save to use in a video?

And it tried multiple times to revise the prompt to get better threads for that.

And it still concluded about half of my threads were worth using for content.

So it's not good at this.

It's not smart enough to do those types of complex reasoning things where I have to make a decision that requires thought.

It's like one last simple way to think of this.

How many seconds would it take for you to answer the question once you've perceived all of the information?

If I show you a picture of a person wearing a mask, what would you think?

It's like one last simple way to think of this.

How many seconds would it take for you to answer the question once you've perceived all of the information?

It's like one last simple way to think of this.

How many seconds would it take for you to answer the question once you've perceived all of the information?

It's like one last simple way to think of this.

How many seconds would it take for you to answer the question once you've perceived all of the information?

It's like one last simple way to think of this.

How many seconds would it take for you to answer the question once you've perceived all of the information?

It's like one last simple way to think of this.

How many seconds would it take for you to answer the question once you've perceived all of the information?

It's like one last simple way to think of this.

How many seconds would it take for you to answer the question once you've perceived all of the information?

It's like one last simple way to think of this.

How many seconds would it take for you to answer the question once you've perceived all of the information?

It's like one last simple way to think of this.

How many seconds would it take for you to answer the question once you've perceived all of the information?

.

answer is under 10 seconds, this model is probably good for it.

If it's over 10 seconds, this model is likely less good for it.

This is the point I'll end on.

And it's similar to the one I started on.

I really want to emphasize the system one point because I'm tired of people not getting it.

If the task requires thinking, this model is not right.

If the task requires classifying, organizing, ranking, real quick decision making, this model is incredible.

It's a system one model.

System two is when you think about a thing.

System one is when you react because your brain thought for you.

It's almost like the reflex.

It's like when you tap your knee and your leg kicks out, that kind of thing.

This model is designed to work like that first part, to be really fast and quick.

If you're thinking of this model in terms of how it replaces the other ones you use, you're probably not thinking about it correctly unless you're doing a lot of structured output work.

The value of this model is that it made so many things that weren't really realistic before way cheaper and faster in a way that is actually kind of cool.

programmer.

But you shouldn't be viewing this as a tool you use in codecs or cloud or even in your terminal.

You should see this like a new library you install or a function that you call.

It is meant to be integrated in the tools that we build, not used as an inference farm to generate code or text or all these other things.

Somebody just said the only way this would replace an LM is if you're using them badly.

I mostly agree.

There are a lot of use case for structured output type stuff.

And this is where it is strongest.

I think this model is really cool.

And I've been enjoying it a lot.

I have a feeling you will too, as long as you go in with the right mindset.

Don't use this to replace fable or judge between complex topics.

Use this like an if statement and you'll have a lot of fun with it.

Let me know how y'all feel.

And until next time, peace nerds.
